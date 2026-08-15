use std::borrow::Cow;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use tiktoken_rs::CoreBPE;

use crate::config::TokenEncoding;
use crate::error::{Error, Result};

/// Bytes buffered before the counter looks for a safe place to tokenize up to.
const FLUSH_THRESHOLD: usize = 64 * 1024;

/// Documents below this size are counted on the calling thread; starting a pool for a
/// handful of chunks costs more than it saves.
const PARALLEL_THRESHOLD: usize = 4 * FLUSH_THRESHOLD;

/// Buffered bytes above which a cut is made at the best boundary available, rather than
/// waiting for an ideal one that some documents never offer.
///
/// A minified bundle, a single-line JSON blob or a base64 payload contains no newline at
/// all, so the preferred split point never occurs and the buffer would otherwise grow
/// until the document ended — holding the whole thing in memory, which is the one thing
/// the streaming design exists to prevent.
const HARD_SPLIT_CEILING: usize = 4 * FLUSH_THRESHOLD;

/// The failure is kept as text because the error type carries sources that cannot be
/// cloned, and a cached value has to be.
type CachedEncoder = std::result::Result<Arc<CoreBPE>, String>;

static CL100K: OnceLock<CachedEncoder> = OnceLock::new();
static O200K: OnceLock<CachedEncoder> = OnceLock::new();

/// The encoding tables, built once per process.
///
/// Constructing one parses a vocabulary of a hundred thousand entries, which is fine once
/// and ruinous per file: an earlier version rebuilt it for every file it ranked.
fn encoder(encoding: TokenEncoding) -> Result<Arc<CoreBPE>> {
    let cell = match encoding {
        TokenEncoding::Cl100kBase => &CL100K,
        TokenEncoding::O200kBase => &O200K,
    };
    cell.get_or_init(|| {
        match encoding {
            TokenEncoding::Cl100kBase => tiktoken_rs::cl100k_base(),
            TokenEncoding::O200kBase => tiktoken_rs::o200k_base(),
        }
        .map(Arc::new)
        .map_err(|e| e.to_string())
    })
    .clone()
    .map_err(Error::Tokenizer)
}

/// Incremental token counter fed by the document as it streams to its destination.
///
/// Only the tokenizer's own running state is held, never the assembled document.
pub struct TokenCounter {
    bpe: Arc<CoreBPE>,
    pending: String,
    total: usize,
    /// Chunks are summed, so they may be tokenized in any order and on any thread.
    pool: Option<Pool>,
    dispatched: usize,
}

struct Pool {
    sender: Option<SyncSender<String>>,
    counted: Arc<AtomicUsize>,
    workers: Vec<JoinHandle<()>>,
}

impl Pool {
    fn start(bpe: &Arc<CoreBPE>) -> Option<Self> {
        let width = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        if width < 2 {
            return None;
        }

        // Bounded so a fast writer cannot queue the whole document in memory.
        let (sender, receiver) = sync_channel::<String>(width * 2);
        let receiver = Arc::new(Mutex::new(receiver));
        let counted = Arc::new(AtomicUsize::new(0));

        let workers = (0..width)
            .map(|_| {
                let bpe = Arc::clone(bpe);
                let receiver: Arc<Mutex<Receiver<String>>> = Arc::clone(&receiver);
                let counted = Arc::clone(&counted);
                std::thread::spawn(move || loop {
                    let chunk = {
                        let guard = match receiver.lock() {
                            Ok(guard) => guard,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        guard.recv()
                    };
                    match chunk {
                        Ok(chunk) => {
                            counted.fetch_add(bpe.encode_ordinary(&chunk).len(), Ordering::Relaxed);
                        }
                        Err(_) => break,
                    }
                })
            })
            .collect();

        Some(Self {
            sender: Some(sender),
            counted,
            workers,
        })
    }

    fn join(mut self) -> usize {
        drop(self.sender.take());
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        self.counted.load(Ordering::Relaxed)
    }
}

impl TokenCounter {
    pub fn new(encoding: TokenEncoding) -> Result<Self> {
        Ok(Self {
            bpe: encoder(encoding)?,
            pending: String::new(),
            total: 0,
            pool: None,
            dispatched: 0,
        })
    }

    pub fn push(&mut self, text: &str) {
        self.pending.push_str(text);
        if self.pending.len() < FLUSH_THRESHOLD {
            return;
        }
        let split = match safe_split(&self.pending) {
            Some(split) => split,
            None if self.pending.len() >= HARD_SPLIT_CEILING => {
                match fallback_split(&self.pending) {
                    Some(split) => split,
                    None => return,
                }
            }
            None => return,
        };
        let tail = self.pending.split_off(split);
        let chunk = std::mem::replace(&mut self.pending, tail);

        self.dispatched += chunk.len();
        if self.pool.is_none() && self.dispatched >= PARALLEL_THRESHOLD {
            self.pool = Pool::start(&self.bpe);
        }

        let chunk = match self.pool.as_ref().and_then(|pool| pool.sender.as_ref()) {
            Some(sender) => match sender.send(chunk) {
                Ok(()) => return,
                // Every worker is gone; counting it here is slower but still correct.
                Err(returned) => {
                    self.pool = None;
                    returned.0
                }
            },
            None => chunk,
        };
        self.total += self.bpe.encode_ordinary(&chunk).len();
    }

    /// Tokenize whatever remains and return the total for the whole document.
    pub fn finish(mut self) -> usize {
        if !self.pending.is_empty() {
            self.total += self.bpe.encode_ordinary(&self.pending).len();
            self.pending.clear();
        }
        if let Some(pool) = self.pool.take() {
            self.total += pool.join();
        }
        self.total
    }
}

/// Position at which the buffer may be cut without changing the tokenizer's result.
///
/// Both reference encodings pre-tokenize a run of newlines as a unit that ends where the
/// next non-whitespace character begins, and byte-pair merges never span a pre-token
/// boundary; cutting exactly there therefore yields the same tokens as one contiguous pass.
/// Position at which the buffer may be cut when no exact boundary exists.
///
/// Whitespace first, because a pre-token rarely spans one; failing that, any character
/// boundary. Either may differ by a token or two from tokenising the buffer whole — a
/// bounded and documented inaccuracy, and a far better trade than buffering a document
/// without limit. Only reachable once the buffer has passed [`HARD_SPLIT_CEILING`], which
/// no ordinary source file does.
fn fallback_split(buffer: &str) -> Option<usize> {
    let bytes = buffer.as_bytes();
    let mut index = buffer.len();
    while index > 1 {
        index -= 1;
        if bytes[index].is_ascii_whitespace() && buffer.is_char_boundary(index) {
            return Some(index);
        }
    }
    // Not one whitespace byte in the whole buffer; cut at the last boundary there is.
    let mut index = buffer.len();
    while index > 1 {
        index -= 1;
        if buffer.is_char_boundary(index) {
            return Some(index);
        }
    }
    None
}

fn safe_split(buffer: &str) -> Option<usize> {
    let bytes = buffer.as_bytes();
    let mut index = buffer.len();
    while index > 1 {
        index -= 1;
        if bytes[index - 1] == b'\n'
            && !bytes[index].is_ascii_whitespace()
            && buffer.is_char_boundary(index)
        {
            return Some(index);
        }
    }
    None
}

/// Token count for a document already held as a single string.
pub fn count_str(encoding: TokenEncoding, text: &str) -> Result<usize> {
    Ok(encoder(encoding)?.encode_ordinary(text).len())
}

/// Token count for a document read back from where it was written.
///
/// Used to confirm the streaming path against an independent measurement, and as the
/// fallback path where incremental counting is unavailable.
pub fn count_reader(encoding: TokenEncoding, mut reader: impl Read) -> Result<usize> {
    let mut counter = TokenCounter::new(encoding)?;
    let mut buffer = vec![0_u8; FLUSH_THRESHOLD];
    let mut carry: Vec<u8> = Vec::new();

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        carry.extend_from_slice(&buffer[..read]);
        let valid = match std::str::from_utf8(&carry) {
            Ok(text) => text.len(),
            Err(e) => e.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
        counter.push(&text);
        carry.drain(..valid);
    }

    if !carry.is_empty() {
        counter.push(&String::from_utf8_lossy(&carry));
    }
    Ok(counter.finish())
}

/// Reassembles UTF-8 text from arbitrarily-chunked byte writes.
#[derive(Debug, Default)]
pub struct Utf8Assembler {
    carry: Vec<u8>,
}

impl Utf8Assembler {
    /// Returns the longest complete UTF-8 prefix available, retaining any split sequence.
    ///
    /// Borrowed whenever it can be, which is almost always: a write that is wholly valid
    /// UTF-8 arriving with nothing carried over is handed straight back. The previous
    /// version copied every byte into `carry` and then copied it out again through
    /// `from_utf8_lossy`, so the whole document was copied twice on the counting path.
    pub fn push<'a>(&'a mut self, bytes: &'a [u8]) -> Cow<'a, str> {
        if self.carry.is_empty() {
            return match std::str::from_utf8(bytes) {
                Ok(text) => Cow::Borrowed(text),
                Err(error) => {
                    let valid = error.valid_up_to();
                    self.carry.extend_from_slice(&bytes[valid..]);
                    // Valid by construction: `valid_up_to` is a boundary of correct text.
                    Cow::Borrowed(std::str::from_utf8(&bytes[..valid]).unwrap_or_default())
                }
            };
        }

        self.carry.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.carry) {
            Ok(text) => text.len(),
            Err(e) => e.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&self.carry[..valid]).into_owned();
        self.carry.drain(..valid);
        Cow::Owned(text)
    }

    /// Flushes any trailing bytes that never formed a complete sequence.
    pub fn finish(&mut self) -> String {
        if self.carry.is_empty() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry.clear();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunked_total(encoding: TokenEncoding, text: &str, chunk: usize) -> usize {
        let mut counter = TokenCounter::new(encoding).unwrap();
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + chunk).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            counter.push(&text[start..end]);
            start = end;
        }
        counter.finish()
    }

    fn sample() -> String {
        let mut text = String::new();
        for index in 0..4000 {
            text.push_str(&format!("fn item_{index}() -> u32 {{ {index} }}\n"));
            if index % 7 == 0 {
                text.push_str("\n\n   \n");
            }
            if index % 11 == 0 {
                text.push_str("日本語のコメント — ünïcode\n");
            }
        }
        text
    }

    #[test]
    fn streaming_matches_whole_document() {
        let text = sample();
        let whole = count_str(TokenEncoding::Cl100kBase, &text).unwrap();
        for chunk in [1, 7, 64, 1023, 65_536] {
            assert_eq!(
                whole,
                chunked_total(TokenEncoding::Cl100kBase, &text, chunk),
                "chunk size {chunk} diverged"
            );
        }
    }

    #[test]
    fn reader_path_matches_whole_document() {
        let text = sample();
        let whole = count_str(TokenEncoding::Cl100kBase, &text).unwrap();
        let streamed = count_reader(TokenEncoding::Cl100kBase, text.as_bytes()).unwrap();
        assert_eq!(whole, streamed);
    }

    #[test]
    fn a_document_without_newlines_does_not_buffer_without_bound() {
        // A minified bundle: no newline anywhere, so the preferred split point never
        // occurs and the buffer used to grow until the document ended — holding the whole
        // thing in memory, which is the one thing the streaming design exists to prevent.
        let unit = "const x=1;let y=2;function f(){return x+y}";
        let text = unit.repeat(HARD_SPLIT_CEILING / unit.len() + FLUSH_THRESHOLD);
        assert!(!text.contains('\n'));

        let mut counter = TokenCounter::new(TokenEncoding::Cl100kBase).unwrap();
        let mut start = 0;
        while start < text.len() {
            let end = (start + FLUSH_THRESHOLD).min(text.len());
            counter.push(&text[start..end]);
            assert!(
                counter.pending.len() <= HARD_SPLIT_CEILING + FLUSH_THRESHOLD,
                "the buffer grew without bound: {} bytes",
                counter.pending.len()
            );
            start = end;
        }
        assert!(counter.finish() > 0);
    }

    #[test]
    fn the_fallback_cut_prefers_whitespace_and_always_finds_a_boundary() {
        assert_eq!(fallback_split("alpha beta"), Some(5));
        // Not one whitespace byte anywhere, and multi-byte characters throughout: the
        // cut still lands on a character boundary rather than inside a sequence.
        let dense = "日本語".repeat(4);
        let split = fallback_split(&dense).unwrap();
        assert!(dense.is_char_boundary(split));
        assert!(split > 0 && split < dense.len());
    }

    #[test]
    fn assembler_returns_borrowed_text_for_ordinary_writes() {
        let mut assembler = Utf8Assembler::default();
        assert!(matches!(
            assembler.push(b"plain ascii"),
            Cow::Borrowed("plain ascii")
        ));
    }

    #[test]
    fn assembler_rejoins_split_sequences() {
        let mut assembler = Utf8Assembler::default();
        let bytes = "日本語".as_bytes();
        let mut out = String::new();
        for byte in bytes {
            out.push_str(assembler.push(&[*byte]).as_ref());
        }
        out.push_str(&assembler.finish());
        assert_eq!(out, "日本語");
    }
}
