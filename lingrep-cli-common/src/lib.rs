mod args;
mod markdown_args;
mod safe_text;
mod terminal;

pub use args::{CorpusInputArgs, DictArgs};
pub use markdown_args::MarkdownReaderArgs;
pub use safe_text::SafeText;
pub use terminal::{TerminalSafeText, is_terminal_safe, sanitize_terminal};
