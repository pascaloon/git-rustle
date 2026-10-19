//! XML tokenization avoids the per-character attribute nodes of the XML grammar.
use super::{MAX_SPANS, Side, Span};
use std::{
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
};
use xmlparser::{ElementEnd, Stream, Token, Tokenizer};

pub(super) fn highlight(
    text: &str,
    starts: &[usize],
    ends: &[usize],
    cancel: &AtomicUsize,
) -> Option<Side> {
    let mut output = Output {
        side: Side {
            spans: Vec::new(),
            lines: vec![0..0; starts.len()],
        },
        starts,
        ends,
        line: 0,
        cancel,
    };
    for token in Tokenizer::from(text) {
        if cancel.load(Ordering::Relaxed) != 0 {
            return None;
        }
        // Keep valid prefix colors in incomplete/malformed files. The remainder
        // stays plain; never guess where an unterminated attribute/comment ends.
        let Ok(token) = token else { break };
        match token {
            Token::Declaration {
                version,
                encoding,
                span,
                ..
            } => {
                output.push(span.start()..version.start(), 2)?;
                output.push(version.range(), 5)?;
                if let Some(encoding) = encoding {
                    output.push(encoding.range(), 1)?;
                }
            }
            Token::ProcessingInstruction {
                target, content, ..
            } => {
                output.push(target.range(), 2)?;
                if let Some(content) = content {
                    output.push(content.range(), 1)?;
                }
            }
            Token::Comment { span, .. } => output.push(span.range(), 0)?,
            Token::ElementStart {
                prefix,
                local,
                span,
            } => {
                let start = if prefix.is_empty() {
                    local.start()
                } else {
                    prefix.start()
                };
                output.push(span.start()..start, 9)?;
                output.push(start..local.end(), 11)?;
            }
            Token::Attribute {
                prefix,
                local,
                value,
                ..
            } => {
                let start = if prefix.is_empty() {
                    local.start()
                } else {
                    prefix.start()
                };
                output.push(start..local.end(), 7)?;
                output.push(local.end()..value.start() - 1, 8)?;
                output.push(value.start() - 1..value.end() + 1, 1)?;
            }
            Token::ElementEnd {
                end: ElementEnd::Close(prefix, local),
                span,
            } => {
                let start = if prefix.is_empty() {
                    local.start()
                } else {
                    prefix.start()
                };
                output.push(span.start()..start, 9)?;
                output.push(start..local.end(), 11)?;
                output.push(local.end()..span.end(), 9)?;
            }
            Token::ElementEnd { span, .. } | Token::DtdEnd { span } => {
                output.push(span.range(), 9)?
            }
            Token::Cdata {
                text: content,
                span,
            } => {
                output.push(span.start()..content.start(), 9)?;
                output.push(content.range(), 1)?;
                output.push(content.end()..span.end(), 9)?;
            }
            Token::Text { text: content } => {
                let mut stream = Stream::from_substr(text, content.range());
                while !stream.at_end() {
                    if cancel.load(Ordering::Relaxed) != 0 {
                        return None;
                    }
                    stream.skip_bytes(|_, b| b != b'&');
                    if stream.at_end() {
                        break;
                    }
                    let start = stream.pos();
                    if stream.consume_reference().is_ok() {
                        output.push(start..stream.pos(), 6)?;
                    } else {
                        // Failed references need not advance the stream.
                        stream = Stream::from_substr(text, start + 1..content.end());
                    }
                }
            }
            Token::DtdStart { name, span, .. } | Token::EmptyDtd { name, span, .. } => {
                output.push(span.start()..name.start(), 2)?;
                output.push(name.range(), 4)?;
                output.push(name.end()..span.end(), 1)?;
            }
            Token::EntityDeclaration { name, span, .. } => {
                output.push(span.start()..name.start(), 2)?;
                output.push(name.range(), 6)?;
                output.push(name.end()..span.end(), 1)?;
            }
        }
    }
    (cancel.load(Ordering::Relaxed) == 0).then_some(output.side)
}

struct Output<'a> {
    side: Side,
    starts: &'a [usize],
    ends: &'a [usize],
    line: usize,
    cancel: &'a AtomicUsize,
}

impl Output<'_> {
    // Token ranges arrive sorted and nonoverlapping, so line splitting is linear.
    fn push(&mut self, range: Range<usize>, style: u8) -> Option<()> {
        while self.line < self.ends.len() && self.ends[self.line] <= range.start {
            self.line += 1;
        }
        while self.line < self.starts.len() && self.starts[self.line] < range.end {
            if self.cancel.load(Ordering::Relaxed) != 0 {
                return None;
            }
            let start = range.start.max(self.starts[self.line]);
            let end = range.end.min(self.ends[self.line]);
            if start < end {
                if self.side.spans.len() == MAX_SPANS {
                    return None;
                }
                if self.side.lines[self.line].is_empty() {
                    self.side.lines[self.line].start = self.side.spans.len();
                }
                self.side.spans.push(Span {
                    range: start - self.starts[self.line]..end - self.starts[self.line],
                    style,
                });
                self.side.lines[self.line].end = self.side.spans.len();
            }
            if range.end <= self.ends[self.line] {
                break;
            }
            self.line += 1;
        }
        Some(())
    }
}
