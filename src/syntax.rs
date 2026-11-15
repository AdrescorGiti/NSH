use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type List = Vec<Item>;

#[derive(Clone, Debug)]
pub struct Item {
    pub chain: Chain,
    pub background: bool,
}

#[derive(Clone, Debug)]
pub struct Chain {
    pub first: Pipeline,
    pub rest: Vec<(AndOr, Pipeline)>,
}

#[derive(Clone, Copy, Debug)]
pub enum AndOr {
    And,
    Or,
}

#[derive(Clone, Debug)]
pub struct Pipeline {
    pub negate: bool,
    pub commands: Vec<Command>,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub kind: Kind,
    pub redirs: Vec<Redir>,
    pub(crate) site: Option<Arc<CommandSite>>,
}

#[derive(Debug)]
pub(crate) struct CommandSite {
    path: Arc<PathBuf>,
    line: usize,
    excerpt: Box<[u8]>,
    truncated: bool,
}

impl CommandSite {
    pub(crate) fn report(&self, message: &str) -> String {
        let path = self.path.to_string_lossy();
        let path = path
            .chars()
            .flat_map(char::escape_debug)
            .collect::<String>();
        let excerpt = String::from_utf8_lossy(&self.excerpt);
        let suffix = if self.truncated { "…" } else { "" };
        format!("{path}:{}: {excerpt:?}{suffix}: {message}", self.line)
    }
}

/// Location metadata is created only for sourced startup/configuration text.
/// A stored function retains its command excerpts rather than its whole file.
pub(crate) struct SourceLocation<'a> {
    source: &'a [u8],
    path: Arc<PathBuf>,
    first_line: usize,
    newlines: Vec<usize>,
}

impl<'a> SourceLocation<'a> {
    pub(crate) fn new(source: &'a [u8], path: &Path) -> Self {
        Self::at_line(source, Arc::new(path.to_path_buf()), 1)
    }

    pub(crate) fn at_line(source: &'a [u8], path: Arc<PathBuf>, first_line: usize) -> Self {
        let newlines = source
            .iter()
            .enumerate()
            .filter_map(|(index, &byte)| (byte == b'\n').then_some(index))
            .collect();
        Self {
            source,
            path,
            first_line,
            newlines,
        }
    }

    fn site(&self, start: usize, end: usize) -> CommandSite {
        let start = start.min(self.source.len());
        let line = self.newlines.partition_point(|&newline| newline < start);
        let line_end = self
            .newlines
            .get(line)
            .copied()
            .unwrap_or(self.source.len());
        let content = self.source[start..line_end.min(end).max(start)].trim_ascii();
        let mut limit = content.len().min(160);
        if let Ok(text) = std::str::from_utf8(content) {
            while !text.is_char_boundary(limit) {
                limit -= 1;
            }
        }
        CommandSite {
            path: Arc::clone(&self.path),
            line: line + self.first_line,
            excerpt: content[..limit].into(),
            truncated: limit < content.len() || end > line_end,
        }
    }

    pub(crate) fn syntax_error(&self, base: usize, error: &ParseError) -> String {
        let position = base + error.offset.unwrap_or_default();
        let line = self.newlines.partition_point(|&newline| newline < position);
        let start = if line == 0 {
            0
        } else {
            self.newlines[line - 1] + 1
        };
        let end = self
            .newlines
            .get(line)
            .copied()
            .unwrap_or(self.source.len());
        self.site(start, end).report(&error.message)
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    Simple(Vec<Word>),
    Group(List),
    Subshell(List),
    If(Vec<(List, List)>, List),
    Loop {
        condition: List,
        body: List,
        until: bool,
    },
    For {
        name: Vec<u8>,
        words: Option<Vec<Word>>,
        body: List,
    },
    Case {
        word: Word,
        arms: Vec<CaseArm>,
    },
    Function(Vec<u8>, Box<Command>),
}

#[derive(Clone, Debug)]
pub struct CaseArm {
    pub patterns: Vec<Word>,
    pub body: List,
    pub fallthrough: bool,
}

#[derive(Clone, Debug)]
pub struct Word(pub Vec<u8>);

#[derive(Clone, Debug)]
pub struct Redir {
    pub fd: i32,
    pub kind: RedirKind,
    pub word: Word,
    here_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub enum RedirKind {
    Read,
    Write,
    Append,
    ReadWrite,
    Clobber,
    DupRead,
    DupWrite,
    Here { body: Vec<u8>, expand: bool },
}

#[derive(Clone, Debug)]
pub struct ParseError {
    pub incomplete: bool,
    pub message: String,
    offset: Option<usize>,
}

impl ParseError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            incomplete: false,
            message: message.into(),
            offset: None,
        }
    }

    fn incomplete(message: impl Into<String>) -> Self {
        Self {
            incomplete: true,
            message: message.into(),
            offset: None,
        }
    }

    fn at(mut self, offset: usize) -> Self {
        self.offset.get_or_insert(offset);
        self
    }

    fn shift_offset(mut self, base: usize) -> Self {
        self.offset = self.offset.map(|offset| base + offset);
        self
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

const MAX_DEPTH: usize = 64;

fn check_depth(depth: usize) -> Result<(), ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError::invalid("shell syntax nesting limit exceeded"));
    }
    Ok(())
}

pub fn parse(source: &[u8], aliases: &HashMap<Vec<u8>, Vec<u8>>) -> Result<List, ParseError> {
    parse_input(source, aliases, false, None).map(|(list, _)| list)
}

pub fn parse_next(
    source: &[u8],
    aliases: &HashMap<Vec<u8>, Vec<u8>>,
) -> Result<(List, usize), ParseError> {
    parse_input(source, aliases, true, None)
}

pub(crate) fn parse_located(
    source: &SourceLocation<'_>,
    offset: usize,
    aliases: &HashMap<Vec<u8>, Vec<u8>>,
    one_input: bool,
) -> Result<(List, usize), ParseError> {
    parse_input(
        &source.source[offset..],
        aliases,
        one_input,
        Some((source, offset)),
    )
}

fn parse_input<'a>(
    source: &'a [u8],
    aliases: &'a HashMap<Vec<u8>, Vec<u8>>,
    one_input: bool,
    location: Option<(&'a SourceLocation<'a>, usize)>,
) -> Result<(List, usize), ParseError> {
    let mut parser = Parser::new(source, aliases, 0);
    parser.location = location;
    parser.lexer.require_continued_line = one_input;
    let mut commands = parser
        .list(&[], &[], one_input)
        .map_err(|error| error.at(parser.lexer.position))?;
    let consumed = parser.last_source_position;
    parser.attach_heres(&mut commands);
    Ok((commands, consumed))
}

/// Locates the end of a complete quoted expansion without interpreting it.
/// A plain variable reference such as "$name" returns None.
pub fn expansion_end(
    source: &[u8],
    start: usize,
    quoted: bool,
) -> Result<Option<usize>, ParseError> {
    scan_expansion(source, start, 0, quoted)
}

fn scan_expansion(
    source: &[u8],
    start: usize,
    depth: usize,
    double_quoted: bool,
) -> Result<Option<usize>, ParseError> {
    let end = match source.get(start..) {
        Some([b'\x60', ..]) => scan_quote(source, start, Quote::Backtick, depth)?,
        Some([b'$', b'\'', ..]) if !double_quoted => {
            scan_quote(source, start + 1, Quote::DollarSingle, depth)?
        }
        Some([b'$', b'{', ..]) => scan_parameter(source, start + 2, depth + 1, double_quoted)?,
        Some([b'$', b'(', b'(', ..]) => scan_arithmetic(source, start + 3, depth + 1)?,
        Some([b'$', b'(', ..]) => {
            check_depth(depth + 1)?;
            let aliases = HashMap::new();
            let base = start + 2;
            let mut parser = Parser::new(&source[base..], &aliases, depth + 1);
            parser
                .list(&[], &[Op::CloseParen], false)
                .map_err(|error| error.shift_offset(base))?;
            parser
                .expect_op(Op::CloseParen)
                .map_err(|error| error.shift_offset(base))?;
            base + parser.last_source_position
        }
        _ => return Ok(None),
    };
    Ok(Some(end))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    Single,
    Double,
    DollarSingle,
    Backtick,
}

fn scan_quote(
    source: &[u8],
    start: usize,
    quote: Quote,
    depth: usize,
) -> Result<usize, ParseError> {
    let marker = match quote {
        Quote::Single | Quote::DollarSingle => b'\'',
        Quote::Double => b'"',
        Quote::Backtick => b'\x60',
    };
    let mut index = start + 1;
    while let Some(&byte) = source.get(index) {
        if byte == marker {
            return Ok(index + 1);
        }
        if byte == b'\\' && quote != Quote::Single {
            index += 2;
            continue;
        }
        if quote == Quote::Double && matches!(byte, b'$' | b'\x60') {
            index = scan_expansion(source, index, depth, true)?.unwrap_or(index + 1);
            continue;
        }
        index += 1;
    }
    Err(ParseError::incomplete("unterminated quote"))
}

fn scan_parameter(
    source: &[u8],
    mut index: usize,
    depth: usize,
    outer_quotes: bool,
) -> Result<usize, ParseError> {
    check_depth(depth)?;
    // Pattern removal operators preserve the special meaning of single quotes
    // in a double-quoted parameter expansion.
    let mut parameter_end = index;
    if source.get(parameter_end) == Some(&b'#') {
        parameter_end += 1;
    }
    if source
        .get(parameter_end)
        .is_some_and(|byte| b"@*#?-$!".contains(byte))
    {
        parameter_end += 1;
    } else {
        while source
            .get(parameter_end)
            .is_some_and(|byte| is_name_byte(*byte))
        {
            parameter_end += 1;
        }
    }
    let pattern = matches!(source.get(parameter_end), Some(b'#' | b'%'));
    while let Some(&byte) = source.get(index) {
        match byte {
            b'}' => return Ok(index + 1),
            b'\\' => index += 2,
            b'\'' if !outer_quotes || pattern => {
                index = scan_quote(source, index, Quote::Single, depth)?;
            }
            b'"' => index = scan_quote(source, index, Quote::Double, depth)?,
            _ => {
                index = scan_expansion(source, index, depth, outer_quotes && !pattern)?
                    .unwrap_or(index + 1)
            }
        }
    }
    Err(ParseError::incomplete("unterminated parameter expansion"))
}

fn scan_arithmetic(source: &[u8], mut index: usize, depth: usize) -> Result<usize, ParseError> {
    check_depth(depth)?;
    let mut parens = 0;
    while let Some(&byte) = source.get(index) {
        match byte {
            b'(' => {
                parens += 1;
                index += 1;
            }
            b')' if parens > 0 => {
                parens -= 1;
                index += 1;
            }
            b')' if source.get(index + 1) == Some(&b')') => return Ok(index + 2),
            b'\\' => index += 2,
            _ => index = scan_expansion(source, index, depth, true)?.unwrap_or(index + 1),
        }
    }
    Err(ParseError::incomplete("unterminated arithmetic expansion"))
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_name(name: &[u8]) -> bool {
    matches!(name.first(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && name.iter().copied().all(is_name_byte)
}

fn is_assignment(word: &[u8]) -> bool {
    word.iter()
        .position(|byte| *byte == b'=')
        .is_some_and(|end| is_name(&word[..end]))
}

fn is_word_separator(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>'
    )
}

fn after_line_continuations(source: &[u8], mut index: usize) -> usize {
    while source
        .get(index..)
        .is_some_and(|rest| rest.starts_with(b"\\\n"))
    {
        index += 2;
    }
    index
}

fn scan_word(
    source: &[u8],
    mut index: usize,
    depth: usize,
    require_continued_line: bool,
) -> Result<(Cow<'_, [u8]>, usize), ParseError> {
    let mut word = Vec::new();
    let start = index;
    let mut segment = start;
    while let Some(&byte) = source.get(index) {
        index = match byte {
            b'\\' => match source.get(index + 1) {
                Some(b'\n') => {
                    word.extend_from_slice(&source[segment..index]);
                    index = after_line_continuations(source, index);
                    segment = index;
                    if require_continued_line && index == source.len() {
                        return Err(ParseError::incomplete("unfinished line continuation"));
                    }
                    continue;
                }
                Some(_) => index + 2,
                None => return Err(ParseError::incomplete("unfinished escape")),
            },
            b'\'' => scan_quote(source, index, Quote::Single, depth)?,
            b'"' => scan_quote(source, index, Quote::Double, depth)?,
            b'\0' => return Err(ParseError::invalid("NUL byte in shell input")),
            _ if is_word_separator(byte) => break,
            b'$' | b'\x60' => scan_expansion(source, index, depth, false)?.unwrap_or(index + 1),
            _ => index + 1,
        };
    }
    if segment == start {
        return Ok((Cow::Borrowed(&source[start..index]), index));
    }
    word.extend_from_slice(&source[segment..index]);
    Ok((Cow::Owned(word), index))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Semi,
    Amp,
    Pipe,
    And,
    Or,
    OpenParen,
    CloseParen,
    CaseEnd,
    CaseFall,
}

impl Op {
    fn spelling(self) -> &'static str {
        match self {
            Self::Semi => ";",
            Self::Amp => "&",
            Self::Pipe => "|",
            Self::And => "&&",
            Self::Or => "||",
            Self::OpenParen => "(",
            Self::CloseParen => ")",
            Self::CaseEnd => ";;",
            Self::CaseFall => ";&",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Redirect {
    Read,
    Write,
    Append,
    ReadWrite,
    Clobber,
    DupRead,
    DupWrite,
    Here,
    HereStrip,
}

impl Redirect {
    fn scan(bytes: &[u8]) -> Option<(Self, usize)> {
        let first = *bytes.first()?;
        if !matches!(first, b'<' | b'>') {
            return None;
        }
        let second = after_line_continuations(bytes, 1);
        Some(match (first, bytes.get(second)) {
            (b'<', Some(b'<')) => {
                let third = after_line_continuations(bytes, second + 1);
                if bytes.get(third) == Some(&b'-') {
                    (Self::HereStrip, third + 1)
                } else {
                    (Self::Here, second + 1)
                }
            }
            (b'>', Some(b'>')) => (Self::Append, second + 1),
            (b'<', Some(b'>')) => (Self::ReadWrite, second + 1),
            (b'>', Some(b'|')) => (Self::Clobber, second + 1),
            (b'<', Some(b'&')) => (Self::DupRead, second + 1),
            (b'>', Some(b'&')) => (Self::DupWrite, second + 1),
            (b'<', _) => (Self::Read, 1),
            _ => (Self::Write, 1),
        })
    }

    fn default_fd(self) -> i32 {
        match self {
            Self::Write | Self::Append | Self::Clobber | Self::DupWrite => 1,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug)]
enum TokenKind<'a> {
    Word(Cow<'a, [u8]>),
    Op(Op),
    Redir(i32, Redirect),
    Newline,
    AliasBlank,
    Eof,
}

#[derive(Debug)]
struct Token<'a> {
    kind: TokenKind<'a>,
    source_start: usize,
    source_position: Option<NonZeroUsize>,
    enclosing_aliases: Vec<Vec<u8>>,
}

impl Token<'_> {
    fn description(&self) -> String {
        match &self.kind {
            TokenKind::Word(word) => format!("{:?}", String::from_utf8_lossy(word)),
            TokenKind::Op(op) => op.spelling().into(),
            TokenKind::Redir(..) => "redirection".into(),
            TokenKind::Newline => "newline".into(),
            TokenKind::AliasBlank => "alias separator".into(),
            TokenKind::Eof => "end of input".into(),
        }
    }
}

// The plain lexer needs at most three queued tokens to recognize name().
// Expanded aliases may push arbitrarily many tokens ahead of that input.
#[derive(Default)]
struct Tokens<'a> {
    plain: [Option<Token<'a>>; 3],
    first: usize,
    len: usize,
    aliases: Option<VecDeque<Token<'a>>>,
}

impl<'a> Tokens<'a> {
    fn len(&self) -> usize {
        self.aliases.as_ref().map_or(self.len, VecDeque::len)
    }

    fn get(&self, index: usize) -> &Token<'a> {
        match &self.aliases {
            Some(queue) => &queue[index],
            None => self.plain[(self.first + index) % self.plain.len()]
                .as_ref()
                .expect("token was prefetched"),
        }
    }

    fn push_back(&mut self, token: Token<'a>) {
        match &mut self.aliases {
            Some(queue) => queue.push_back(token),
            None => {
                assert!(self.len < self.plain.len(), "plain lexer lookahead");
                let index = (self.first + self.len) % self.plain.len();
                self.plain[index] = Some(token);
                self.len += 1;
            }
        }
    }

    fn pop_front(&mut self) -> Option<Token<'a>> {
        if let Some(queue) = &mut self.aliases {
            return queue.pop_front();
        }
        if self.len == 0 {
            return None;
        }
        let token = self.plain[self.first].take();
        self.first = (self.first + 1) % self.plain.len();
        self.len -= 1;
        token
    }

    fn prepend(&mut self, tokens: Vec<Token<'a>>) {
        if tokens.is_empty() {
            return;
        }
        if self.aliases.is_none() {
            let mut queue = VecDeque::with_capacity(tokens.len() + self.len);
            while let Some(token) = self.pop_front() {
                queue.push_back(token);
            }
            self.aliases = Some(queue);
        }
        let queue = self.aliases.as_mut().expect("alias queue initialized");
        for token in tokens.into_iter().rev() {
            queue.push_front(token);
        }
    }
}

struct PendingHere {
    delimiter: Vec<u8>,
    expand: bool,
    strip_tabs: bool,
}

struct Lexer<'a> {
    source: &'a [u8],
    position: usize,
    depth: usize,
    here_bodies: Vec<Vec<u8>>,
    pending_heres: Vec<PendingHere>,
    require_continued_line: bool,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a [u8], depth: usize) -> Self {
        Self {
            source,
            position: 0,
            depth,
            here_bodies: Vec::new(),
            pending_heres: Vec::new(),
            require_continued_line: false,
        }
    }

    fn next(&mut self) -> Result<Token<'a>, ParseError> {
        loop {
            match &self.source[self.position..] {
                [b' ' | b'\t', ..] => self.position += 1,
                [b'\\', b'\n', ..] => {
                    self.position = after_line_continuations(self.source, self.position);
                    if self.require_continued_line && self.position == self.source.len() {
                        return Err(ParseError::incomplete("unfinished line continuation"));
                    }
                }
                [b'#', ..] => {
                    self.position = self.source[self.position..]
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(self.source.len(), |end| self.position + end);
                }
                _ => break,
            }
        }
        let start = self.position;
        let rest = &self.source[start..];
        let kind = if let Some((redirect, size)) = Redirect::scan(rest) {
            self.position += size;
            TokenKind::Redir(redirect.default_fd(), redirect)
        } else {
            match rest {
                [] => {
                    if !self.pending_heres.is_empty() {
                        return Err(ParseError::incomplete("missing here-document body"));
                    }
                    TokenKind::Eof
                }
                [b'\n', ..] => {
                    self.position += 1;
                    self.read_heres()?;
                    TokenKind::Newline
                }
                [first @ (b';' | b'&' | b'|'), ..] => {
                    let next = after_line_continuations(rest, 1);
                    match (first, rest.get(next)) {
                        (b';', Some(b';')) => self.operator(Op::CaseEnd, next + 1),
                        (b';', Some(b'&')) => self.operator(Op::CaseFall, next + 1),
                        (b'&', Some(b'&')) => self.operator(Op::And, next + 1),
                        (b'|', Some(b'|')) => self.operator(Op::Or, next + 1),
                        (b';', _) => self.operator(Op::Semi, 1),
                        (b'&', _) => self.operator(Op::Amp, 1),
                        _ => self.operator(Op::Pipe, 1),
                    }
                }
                [b'(', ..] => self.operator(Op::OpenParen, 1),
                [b')', ..] => self.operator(Op::CloseParen, 1),
                _ => {
                    let (word, end) = scan_word(
                        self.source,
                        self.position,
                        self.depth,
                        self.require_continued_line,
                    )
                    .map_err(|error| error.at(start))?;
                    self.position = end;
                    match Redirect::scan(&self.source[end..]) {
                        Some((redirect, size)) if word.iter().all(u8::is_ascii_digit) => {
                            let fd = word
                                .iter()
                                .try_fold(0i32, |fd, &digit| {
                                    fd.checked_mul(10)?.checked_add(i32::from(digit - b'0'))
                                })
                                .ok_or_else(|| {
                                    ParseError::invalid("file descriptor out of range")
                                })?;
                            self.position += size;
                            TokenKind::Redir(fd, redirect)
                        }
                        _ => TokenKind::Word(word),
                    }
                }
            }
        };
        Ok(Token {
            kind,
            source_start: start,
            source_position: NonZeroUsize::new(self.position + 1),
            enclosing_aliases: Vec::new(),
        })
    }

    fn operator(&mut self, op: Op, width: usize) -> TokenKind<'a> {
        self.position += width;
        TokenKind::Op(op)
    }

    fn register_here(&mut self, pending: PendingHere) -> usize {
        let index = self.here_bodies.len() + self.pending_heres.len();
        self.pending_heres.push(pending);
        index
    }

    fn read_heres(&mut self) -> Result<(), ParseError> {
        for here in std::mem::take(&mut self.pending_heres) {
            let mut body = Vec::new();
            loop {
                let mut logical_line = Vec::new();
                let ended = loop {
                    if self.position == self.source.len() {
                        return Err(ParseError::incomplete(format!(
                            "missing here-document delimiter {:?}",
                            String::from_utf8_lossy(&here.delimiter),
                        )));
                    }
                    let remainder = &self.source[self.position..];
                    let newline = remainder.iter().position(|byte| *byte == b'\n');
                    let end = newline.unwrap_or(remainder.len());
                    self.position += end + usize::from(newline.is_some());
                    let line = &remainder[..end];
                    // An expandable here-document joins logical lines when the
                    // final backslash has not itself been escaped.
                    if here.expand
                        && newline.is_some()
                        && line.iter().rev().take_while(|byte| **byte == b'\\').count() % 2 == 1
                    {
                        logical_line.extend_from_slice(&line[..line.len() - 1]);
                    } else {
                        logical_line.extend_from_slice(line);
                        break newline.is_some();
                    }
                };
                let line = if here.strip_tabs {
                    strip_leading_tabs(&logical_line)
                } else {
                    &logical_line
                };
                if line == here.delimiter {
                    break;
                }
                body.extend_from_slice(line);
                if ended {
                    body.push(b'\n');
                }
            }
            self.here_bodies.push(body);
        }
        Ok(())
    }
}

fn strip_leading_tabs(line: &[u8]) -> &[u8] {
    let first = line
        .iter()
        .position(|byte| *byte != b'\t')
        .unwrap_or(line.len());
    &line[first..]
}

/// Detect pasted Markdown before running a configuration file. The lexer
/// consumes quoted words and registered here-documents as single units, so a
/// fence embedded in literal shell data stays literal.
pub fn configuration_fence(source: &[u8]) -> Option<(usize, &[u8])> {
    let mut lexer = Lexer::new(source, 0);
    let mut here = None;
    loop {
        let mut position = lexer.position;
        loop {
            match &source[position..] {
                [b' ' | b'\t', ..] => position += 1,
                [b'\\', b'\n', ..] => position += 2,
                _ => break,
            }
        }
        let rest = &source[position..];
        if rest.starts_with(&[b'`'; 3]) || rest.starts_with(b"~~~") {
            let before = &source[..position];
            let line_start = before
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map_or(0, |newline| newline + 1);
            if before[line_start..]
                .iter()
                .all(|byte| matches!(byte, b' ' | b'\t'))
            {
                let end = rest
                    .iter()
                    .position(|&byte| byte == b'\n')
                    .unwrap_or(rest.len());
                let line = rest[..end].trim_ascii_end();
                let markers = line.iter().take_while(|byte| **byte == rest[0]).count();
                let info = line[markers..].trim_ascii_start();
                if info.is_empty() || info[0].is_ascii_alphabetic() && !info.contains(&rest[0]) {
                    return Some((
                        1 + before.iter().filter(|&&byte| byte == b'\n').count(),
                        line,
                    ));
                }
            }
        }
        // Leave ordinary syntax errors to the parser and its positioned diagnostics.
        match (lexer.next().ok()?.kind, here.take()) {
            (TokenKind::Eof, _) => return None,
            (TokenKind::Word(word), Some(strip_tabs)) => {
                let (delimiter, expand) = here_delimiter(&word).ok()?;
                lexer.register_here(PendingHere {
                    delimiter,
                    expand,
                    strip_tabs,
                });
            }
            (TokenKind::Redir(_, Redirect::Here | Redirect::HereStrip), Some(_)) => return None,
            (TokenKind::Redir(_, Redirect::Here), None) => here = Some(false),
            (TokenKind::Redir(_, Redirect::HereStrip), None) => here = Some(true),
            (_, Some(_)) => return None,
            _ => {}
        }
    }
}

fn here_delimiter(word: &[u8]) -> Result<(Vec<u8>, bool), ParseError> {
    let mut delimiter = Vec::new();
    let mut quote = None;
    let mut expand = true;
    let mut index = 0;
    while let Some(&byte) = word.get(index) {
        match (quote, byte) {
            (None, b'$') if word.get(index + 1) == Some(&b'\'') => {
                let end = scan_quote(word, index + 1, Quote::DollarSingle, 0)?;
                delimiter.extend(
                    decode_dollar_single(&word[index + 2..end - 1]).map_err(ParseError::invalid)?,
                );
                index = end;
                expand = false;
                continue;
            }
            (None, b'$' | b'\x60') => {
                if let Some(end) = scan_expansion(word, index, 0, false)? {
                    delimiter.extend_from_slice(&word[index..end]);
                    index = end;
                    continue;
                }
                delimiter.push(byte);
            }
            (None, b'\'' | b'"') => {
                quote = Some(byte);
                expand = false;
            }
            (Some(mark), _) if byte == mark => quote = None,
            (None | Some(b'"'), b'\\') => {
                let next = word.get(index + 1).copied();
                if quote.is_none() || next.is_some_and(|byte| b"$\x60\"\\\n".contains(&byte)) {
                    if let Some(next) = next.filter(|byte| *byte != b'\n') {
                        delimiter.push(next);
                    }
                    expand = false;
                    index += 2;
                    continue;
                }
                delimiter.push(byte);
            }
            _ => delimiter.push(byte),
        }
        index += 1;
    }
    Ok((delimiter, expand))
}

pub fn decode_dollar_single(input: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut rest = input;
    let mut output = Vec::new();
    while let Some((&byte, following)) = rest.split_first() {
        rest = following;
        if byte != b'\\' {
            output.push(byte);
            continue;
        }
        let Some((&code, following)) = rest.split_first() else {
            output.push(byte);
            break;
        };
        rest = following;
        let decoded = match code {
            b'a' => b'\x07',
            b'b' => b'\x08',
            b'e' => b'\x1b',
            b'f' => b'\x0c',
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => b'\x0b',
            b'\\' | b'\'' | b'"' | b'?' => code,
            b'c' => {
                let Some((&control, following)) = rest.split_first() else {
                    return Err("control escape requires a character");
                };
                rest = following;
                if control == b'\\' && rest.first() == Some(&b'\\') {
                    rest = &rest[1..];
                }
                if control == b'?' {
                    0x7f
                } else {
                    control.to_ascii_uppercase() & 0x1f
                }
            }
            b'0'..=b'7' => {
                let (decoded, count) = digits(rest, 8, 2, code - b'0');
                rest = &rest[count..];
                decoded
            }
            b'x' => {
                let (decoded, count) = digits(rest, 16, 2, 0);
                if count == 0 {
                    return Err("hex escape requires digits");
                }
                rest = &rest[count..];
                decoded
            }
            _ => {
                output.push(b'\\');
                code
            }
        };
        if decoded == 0 {
            // Issue 8 permits discarding the rest of this quoted sequence at NUL.
            break;
        }
        output.push(decoded);
    }
    Ok(output)
}

fn digits(input: &[u8], radix: u32, max: usize, initial: u8) -> (u8, usize) {
    let mut result = initial;
    let mut count = 0;
    for &byte in input.iter().take(max) {
        let Some(digit) = char::from(byte).to_digit(radix) else {
            break;
        };
        result = result.wrapping_mul(radix as u8).wrapping_add(digit as u8);
        count += 1;
    }
    (result, count)
}

const RESERVED: &[&[u8]] = &[
    b"!", b"{", b"}", b"case", b"do", b"done", b"elif", b"else", b"esac", b"fi", b"for", b"if",
    b"in", b"then", b"until", b"while",
];

struct Parser<'a> {
    lexer: Lexer<'a>,
    aliases: &'a HashMap<Vec<u8>, Vec<u8>>,
    tokens: Tokens<'a>,
    depth: usize,
    last_source_position: usize,
    force_alias: bool,
    location: Option<(&'a SourceLocation<'a>, usize)>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a [u8], aliases: &'a HashMap<Vec<u8>, Vec<u8>>, depth: usize) -> Self {
        Self {
            lexer: Lexer::new(source, depth),
            aliases,
            tokens: Tokens::default(),
            depth,
            last_source_position: 0,
            force_alias: false,
            location: None,
        }
    }

    fn peek(&mut self) -> Result<&Token<'a>, ParseError> {
        loop {
            if self.tokens.len() == 0 {
                self.tokens.push_back(self.lexer.next()?);
            }
            if !matches!(self.tokens.get(0).kind, TokenKind::AliasBlank) {
                return Ok(self.tokens.get(0));
            }
            self.tokens.pop_front();
            self.force_alias = true;
        }
    }

    fn take(&mut self) -> Result<Token<'a>, ParseError> {
        self.peek()?;
        let token = self.tokens.pop_front().expect("peek filled token queue");
        if let Some(position) = token.source_position {
            self.last_source_position = position.get() - 1;
        }
        self.force_alias = false;
        Ok(token)
    }

    fn at_op(&mut self, op: Op) -> Result<bool, ParseError> {
        Ok(matches!(self.peek()?.kind, TokenKind::Op(current) if current == op))
    }

    fn eat_op(&mut self, op: Op) -> Result<bool, ParseError> {
        if self.at_op(op)? {
            self.take()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn expect_op(&mut self, op: Op) -> Result<(), ParseError> {
        if self.eat_op(op)? {
            return Ok(());
        }
        Err(self.expected(op.spelling())?)
    }

    fn at_word(&mut self, text: &[u8]) -> Result<bool, ParseError> {
        Ok(matches!(&self.peek()?.kind, TokenKind::Word(word) if word.as_ref() == text))
    }

    fn eat_word(&mut self, text: &[u8]) -> Result<bool, ParseError> {
        if self.at_word(text)? {
            self.take()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn expect_word(&mut self, text: &[u8]) -> Result<(), ParseError> {
        if self.eat_word(text)? {
            return Ok(());
        }
        Err(self.expected(&String::from_utf8_lossy(text))?)
    }

    fn word(&mut self, description: &str) -> Result<Word, ParseError> {
        if !matches!(self.peek()?.kind, TokenKind::Word(_)) {
            return Err(self.expected(description)?);
        }
        let TokenKind::Word(word) = self.take()?.kind else {
            unreachable!("peek checked word");
        };
        Ok(Word(word.into_owned()))
    }

    fn expected(&mut self, expected: &str) -> Result<ParseError, ParseError> {
        let token = self.peek()?;
        let message = format!("expected {expected}, found {}", token.description());
        if matches!(token.kind, TokenKind::Eof) {
            return Ok(ParseError::incomplete(message));
        }
        Ok(ParseError::invalid(message).at(token.source_start))
    }

    fn skip_newlines(&mut self) -> Result<(), ParseError> {
        while matches!(self.peek()?.kind, TokenKind::Newline) {
            self.take()?;
        }
        Ok(())
    }

    fn expand_alias(&mut self, command_position: bool) -> Result<(), ParseError> {
        if self.aliases.is_empty() {
            return Ok(());
        }
        loop {
            self.peek()?;
            if !(command_position || self.force_alias) {
                return Ok(());
            }
            let current = self.tokens.get(0);
            let TokenKind::Word(word) = &current.kind else {
                return Ok(());
            };
            let Some(value) = self.aliases.get(word.as_ref()) else {
                return Ok(());
            };
            if (command_position && (RESERVED.contains(&word.as_ref()) || is_assignment(word)))
                || current
                    .enclosing_aliases
                    .iter()
                    .any(|ancestor| ancestor == word.as_ref())
                || !word
                    .iter()
                    .all(|&byte| is_name_byte(byte) || b"!%,@+-.:".contains(&byte))
            {
                return Ok(());
            }
            let mut enclosing_aliases = current.enclosing_aliases.clone();
            let source_start = current.source_start;
            enclosing_aliases.push(word.to_vec());
            check_depth(self.depth + enclosing_aliases.len())?;
            let mut lexer = Lexer::new(value, self.depth + enclosing_aliases.len());
            let mut expansion = Vec::new();
            loop {
                let mut token = lexer.next().map_err(|mut error| {
                    error.offset = Some(source_start);
                    error
                })?;
                if matches!(token.kind, TokenKind::Eof) {
                    break;
                }
                token.source_position = None;
                token.source_start = source_start;
                token.enclosing_aliases = enclosing_aliases.clone();
                expansion.push(token);
            }
            if value
                .last()
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            {
                expansion.push(Token {
                    kind: TokenKind::AliasBlank,
                    source_start,
                    source_position: None,
                    enclosing_aliases: Vec::new(),
                });
            }
            self.take()?;
            self.tokens.prepend(expansion);
        }
    }

    fn list(
        &mut self,
        word_stops: &[&[u8]],
        op_stops: &[Op],
        one_input: bool,
    ) -> Result<List, ParseError> {
        let mut commands = Vec::new();
        loop {
            self.expand_alias(true)?;
            if matches!(self.peek()?.kind, TokenKind::Newline) {
                let newline = self.take()?;
                if one_input && newline.source_position.is_some() {
                    return Ok(commands);
                }
                continue;
            }
            if matches!(self.peek()?.kind, TokenKind::Eof) {
                self.take()?;
                return Ok(commands);
            }
            if self.at_stop(word_stops, op_stops)? {
                return Ok(commands);
            }

            let chain = self.chain()?;
            let background = self.eat_op(Op::Amp)?;
            commands.push(Item { chain, background });
            if background || self.eat_op(Op::Semi)? {
                continue;
            }

            if self.at_stop(word_stops, op_stops)? {
                return Ok(commands);
            }
            if !matches!(self.peek()?.kind, TokenKind::Newline | TokenKind::Eof) {
                return Err(self.expected("command separator")?);
            }
        }
    }

    fn at_stop(&mut self, words: &[&[u8]], ops: &[Op]) -> Result<bool, ParseError> {
        Ok(match &self.peek()?.kind {
            TokenKind::Word(word) => words.contains(&word.as_ref()),
            TokenKind::Op(op) => ops.contains(op),
            _ => false,
        })
    }

    fn nonempty_list(&mut self, words: &[&[u8]], ops: &[Op]) -> Result<List, ParseError> {
        let list = self.list(words, ops, false)?;
        if list.is_empty() {
            return Err(self.expected("command")?);
        }
        Ok(list)
    }

    fn chain(&mut self) -> Result<Chain, ParseError> {
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        loop {
            let op = match self.peek()?.kind {
                TokenKind::Op(Op::And) => AndOr::And,
                TokenKind::Op(Op::Or) => AndOr::Or,
                _ => break,
            };
            self.take()?;
            self.skip_newlines()?;
            rest.push((op, self.pipeline()?));
        }
        Ok(Chain { first, rest })
    }

    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        self.expand_alias(true)?;
        let negate = self.eat_word(b"!")?;
        let mut commands = vec![self.command()?];
        while self.eat_op(Op::Pipe)? {
            self.skip_newlines()?;
            commands.push(self.command()?);
        }
        Ok(Pipeline { negate, commands })
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        check_depth(self.depth + 1)?;
        let start = self.command_start()?;
        self.depth += 1;
        let mut command = self.command_inner();
        self.depth -= 1;
        if let Some(start) = start {
            match &mut command {
                Ok(command) => command.site = self.command_site(start),
                Err(error) => {
                    error.offset.get_or_insert(start);
                }
            }
        }
        command
    }

    fn command_start(&mut self) -> Result<Option<usize>, ParseError> {
        if self.location.is_some() {
            return Ok(Some(self.peek()?.source_start));
        }
        Ok(None)
    }

    fn command_site(&self, start: usize) -> Option<Arc<CommandSite>> {
        let (source, base) = self.location?;
        Some(Arc::new(
            source.site(base + start, base + self.last_source_position),
        ))
    }

    fn command_inner(&mut self) -> Result<Command, ParseError> {
        self.expand_alias(true)?;
        if let Some(kind) = self.compound()? {
            return Ok(Command {
                kind,
                redirs: self.redirs()?,
                site: None,
            });
        }
        if self.is_function()? {
            let name = self.word("function name")?.0;
            self.expect_op(Op::OpenParen)?;
            self.expect_op(Op::CloseParen)?;
            self.skip_newlines()?;
            let start = self.command_start()?;
            let Some(kind) = self.compound()? else {
                return Err(self.expected("compound function body")?);
            };
            let body = Command {
                kind,
                redirs: self.redirs()?,
                site: start.and_then(|start| self.command_site(start)),
            };
            return Ok(Command {
                kind: Kind::Function(name, Box::new(body)),
                redirs: Vec::new(),
                site: None,
            });
        }
        self.simple()
    }

    fn is_function(&mut self) -> Result<bool, ParseError> {
        if !matches!(&self.peek()?.kind, TokenKind::Word(word) if is_name(word)) {
            return Ok(false);
        }
        let mut seen = 0;
        let mut index = 1;
        loop {
            if self.tokens.len() <= index {
                self.tokens.push_back(self.lexer.next()?);
            }
            let kind = &self.tokens.get(index).kind;
            if matches!(kind, TokenKind::AliasBlank) {
                index += 1;
                continue;
            }
            if seen == 0 && matches!(kind, TokenKind::Op(Op::OpenParen)) {
                seen += 1;
                index += 1;
                continue;
            }
            return Ok(seen == 1 && matches!(kind, TokenKind::Op(Op::CloseParen)));
        }
    }

    fn compound(&mut self) -> Result<Option<Kind>, ParseError> {
        let token = &self.peek()?.kind;
        if matches!(token, TokenKind::Op(Op::OpenParen)) {
            self.take()?;
            let body = self.nonempty_list(&[], &[Op::CloseParen])?;
            self.expect_op(Op::CloseParen)?;
            return Ok(Some(Kind::Subshell(body)));
        }
        let TokenKind::Word(word) = token else {
            return Ok(None);
        };
        match word.as_ref() {
            b"{" => {
                self.take()?;
                let body = self.nonempty_list(&[b"}"], &[])?;
                self.expect_word(b"}")?;
                Ok(Some(Kind::Group(body)))
            }
            b"if" => {
                self.take()?;
                self.if_clause().map(Some)
            }
            b"while" | b"until" => {
                let until = word.as_ref() == b"until";
                self.take()?;
                let condition = self.nonempty_list(&[b"do"], &[])?;
                let body = self.do_group()?;
                Ok(Some(Kind::Loop {
                    condition,
                    body,
                    until,
                }))
            }
            b"for" => {
                self.take()?;
                self.for_clause().map(Some)
            }
            b"case" => {
                self.take()?;
                self.case_clause().map(Some)
            }
            _ => Ok(None),
        }
    }

    fn if_clause(&mut self) -> Result<Kind, ParseError> {
        let mut branches = Vec::new();
        loop {
            let condition = self.nonempty_list(&[b"then"], &[])?;
            self.expect_word(b"then")?;
            let body = self.nonempty_list(&[b"elif", b"else", b"fi"], &[])?;
            branches.push((condition, body));
            if !self.eat_word(b"elif")? {
                break;
            }
        }
        let fallback = if self.eat_word(b"else")? {
            self.nonempty_list(&[b"fi"], &[])?
        } else {
            Vec::new()
        };
        self.expect_word(b"fi")?;
        Ok(Kind::If(branches, fallback))
    }

    fn do_group(&mut self) -> Result<List, ParseError> {
        self.expect_word(b"do")?;
        let body = self.nonempty_list(&[b"done"], &[])?;
        self.expect_word(b"done")?;
        Ok(body)
    }

    fn for_clause(&mut self) -> Result<Kind, ParseError> {
        let Word(name) = self.word("for loop variable")?;
        if !is_name(&name) {
            return Err(ParseError::invalid("invalid for loop variable"));
        }
        let newline_before_in = matches!(self.peek()?.kind, TokenKind::Newline);
        self.skip_newlines()?;
        let words = if self.eat_word(b"in")? {
            let mut words = Vec::new();
            while matches!(self.peek()?.kind, TokenKind::Word(_)) {
                words.push(self.word("for loop value")?);
            }
            if !self.eat_op(Op::Semi)? && !matches!(self.peek()?.kind, TokenKind::Newline) {
                return Err(self.expected("for loop separator")?);
            }
            Some(words)
        } else {
            if !newline_before_in {
                self.eat_op(Op::Semi)?;
            }
            None
        };
        self.skip_newlines()?;
        let body = self.do_group()?;
        Ok(Kind::For { name, words, body })
    }

    fn case_clause(&mut self) -> Result<Kind, ParseError> {
        let word = self.word("case value")?;
        self.skip_newlines()?;
        self.expect_word(b"in")?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines()?;
            if self.eat_word(b"esac")? {
                break;
            }
            self.eat_op(Op::OpenParen)?;
            let mut patterns = vec![self.word("case pattern")?];
            while self.eat_op(Op::Pipe)? {
                patterns.push(self.word("case pattern")?);
            }
            self.expect_op(Op::CloseParen)?;
            let body = self.list(&[b"esac"], &[Op::CaseEnd, Op::CaseFall], false)?;
            let fallthrough = self.eat_op(Op::CaseFall)?;
            if !fallthrough && !self.eat_op(Op::CaseEnd)? && !self.at_word(b"esac")? {
                return Err(self.expected("case terminator")?);
            }
            arms.push(CaseArm {
                patterns,
                body,
                fallthrough,
            });
        }
        Ok(Kind::Case { word, arms })
    }

    fn simple(&mut self) -> Result<Command, ParseError> {
        let mut words = Vec::new();
        let mut redirs = Vec::new();
        let mut saw_command = false;
        loop {
            self.expand_alias(!saw_command)?;
            match &self.peek()?.kind {
                TokenKind::Redir(..) => redirs.push(self.redir()?),
                TokenKind::Word(word) => {
                    if words.is_empty() && redirs.is_empty() && RESERVED.contains(&word.as_ref()) {
                        return Err(self.expected("command")?);
                    }
                    if !saw_command && !is_assignment(word) {
                        saw_command = true;
                    }
                    let TokenKind::Word(word) = self.take()?.kind else {
                        unreachable!("peek checked command argument");
                    };
                    words.push(Word(word.into_owned()));
                }
                _ => break,
            }
        }
        if words.is_empty() && redirs.is_empty() {
            return Err(self.expected("command")?);
        }
        Ok(Command {
            kind: Kind::Simple(words),
            redirs,
            site: None,
        })
    }

    fn redirs(&mut self) -> Result<Vec<Redir>, ParseError> {
        let mut redirs = Vec::new();
        while matches!(self.peek()?.kind, TokenKind::Redir(..)) {
            redirs.push(self.redir()?);
        }
        Ok(redirs)
    }

    fn redir(&mut self) -> Result<Redir, ParseError> {
        let TokenKind::Redir(fd, redirect) = self.take()?.kind else {
            unreachable!("caller checked redirection");
        };
        let word = self.word("redirection target")?;
        let mut here_index = None;
        let kind = match redirect {
            Redirect::Read => RedirKind::Read,
            Redirect::Write => RedirKind::Write,
            Redirect::Append => RedirKind::Append,
            Redirect::ReadWrite => RedirKind::ReadWrite,
            Redirect::Clobber => RedirKind::Clobber,
            Redirect::DupRead => RedirKind::DupRead,
            Redirect::DupWrite => RedirKind::DupWrite,
            Redirect::Here | Redirect::HereStrip => {
                let (delimiter, expand) = here_delimiter(&word.0)?;
                let strip_tabs = redirect == Redirect::HereStrip;
                here_index = Some(self.lexer.register_here(PendingHere {
                    delimiter,
                    expand,
                    strip_tabs,
                }));
                RedirKind::Here {
                    body: Vec::new(),
                    expand,
                }
            }
        };
        Ok(Redir {
            fd,
            kind,
            word,
            here_index,
        })
    }

    fn attach_heres(self, list: &mut List) {
        if !self.lexer.here_bodies.is_empty() {
            attach_list(
                list,
                &mut self
                    .lexer
                    .here_bodies
                    .into_iter()
                    .map(Some)
                    .collect::<Vec<_>>(),
            );
        }
    }
}

fn attach_list(list: &mut List, bodies: &mut [Option<Vec<u8>>]) {
    for item in list {
        for pipeline in std::iter::once(&mut item.chain.first)
            .chain(item.chain.rest.iter_mut().map(|(_, pipeline)| pipeline))
        {
            for command in &mut pipeline.commands {
                attach_command(command, bodies);
            }
        }
    }
}

fn attach_command(command: &mut Command, bodies: &mut [Option<Vec<u8>>]) {
    for redir in &mut command.redirs {
        if let (Some(index), RedirKind::Here { body, .. }) =
            (redir.here_index.take(), &mut redir.kind)
        {
            *body = bodies[index].take().expect("one body per here-document");
        }
    }
    match &mut command.kind {
        Kind::Simple(_) => {}
        Kind::Group(list) | Kind::Subshell(list) => attach_list(list, bodies),
        Kind::If(branches, fallback) => {
            for (condition, body) in branches {
                attach_list(condition, bodies);
                attach_list(body, bodies);
            }
            attach_list(fallback, bodies);
        }
        Kind::Loop {
            condition, body, ..
        } => {
            attach_list(condition, bodies);
            attach_list(body, bodies);
        }
        Kind::For { body, .. } => attach_list(body, bodies),
        Kind::Case { arms, .. } => {
            for arm in arms {
                attach_list(&mut arm.body, bodies);
            }
        }
        Kind::Function(_, body) => attach_command(body, bodies),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(input: &[u8]) -> List {
        parse(input, &HashMap::new()).expect("valid shell input")
    }

    #[test]
    fn parses_compound_commands_and_issue_eight_case() {
        let commands = tree(b"f() { for x in a b; do if true; then echo \"$x\"; fi; done; }; case x in (x) f ;& *) : ;; esac");
        let Kind::Function(name, body) = &commands[0].chain.first.commands[0].kind else {
            panic!("expected a function");
        };
        assert_eq!(name, b"f");
        let Kind::Group(group) = &body.kind else {
            panic!("expected a group")
        };
        let Kind::For { body, .. } = &group[0].chain.first.commands[0].kind else {
            panic!("expected a for loop");
        };
        assert!(matches!(body[0].chain.first.commands[0].kind, Kind::If(..)));
        let Kind::Case { arms, .. } = &commands[1].chain.first.commands[0].kind else {
            panic!("expected case");
        };
        assert!(arms[0].fallthrough);
        assert!(!arms[1].fallthrough);
    }

    #[test]
    fn heredocs_follow_source_order_and_keep_quoting() {
        let commands = tree(b"cat <<one 3<<-'two' 2>&1\n$x\none\n\t$x\n\ttwo\necho next\n");
        let redirs = &commands[0].chain.first.commands[0].redirs;
        assert!(
            matches!(&redirs[0].kind, RedirKind::Here { body, expand: true } if body == b"$x\n")
        );
        assert!(
            matches!(&redirs[1].kind, RedirKind::Here { body, expand: false } if body == b"$x\n")
        );
        assert_eq!(redirs[1].fd, 3);
        assert_eq!(redirs[2].fd, 2);
        assert!(matches!(redirs[2].kind, RedirKind::DupWrite));
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn keeps_nested_expansions_as_one_word() {
        let commands = tree(br#"echo "$(case foo in foo) printf '%s' "$(echo 'a b')" ;; esac)" $((1+(2*3))) $'a\nb'"#);
        let Kind::Simple(words) = &commands[0].chain.first.commands[0].kind else {
            panic!("expected a simple command");
        };
        assert_eq!(words.len(), 4);
        assert_eq!(words[2].0, b"$((1+(2*3)))");
        assert_eq!(words[3].0, br"$'a\nb'");
    }

    #[test]
    fn aliases_expand_command_words_and_a_trailing_blank() {
        let aliases = HashMap::from([
            (b"hello".to_vec(), b"printf '%s' ".to_vec()),
            (b"world".to_vec(), b"there".to_vec()),
            (b"recur".to_vec(), b"recur".to_vec()),
        ]);
        let commands = parse(b"hello world; recur; echo world", &aliases).unwrap();
        let word = |command: usize, index: usize| {
            let Kind::Simple(words) = &commands[command].chain.first.commands[0].kind else {
                panic!("expected a simple command");
            };
            words[index].0.as_slice()
        };
        assert_eq!(word(0, 0), b"printf");
        assert_eq!(word(0, 2), b"there");
        assert_eq!(word(1, 0), b"recur");
        assert_eq!(word(2, 1), b"world");
    }

    #[test]
    fn parses_only_one_complete_top_level_input() {
        let input = b"if true; then\n  cat <<EOF\ninside\nEOF\nfi\nsyntax ) error\n";
        let (commands, used) = parse_next(input, &HashMap::new()).unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(&input[used..], b"syntax ) error\n");
        assert!(
            !parse_next(&input[used..], &HashMap::new())
                .unwrap_err()
                .incomplete
        );
    }

    #[test]
    fn distinguishes_incomplete_and_invalid_input() {
        assert!(
            parse(b"if true; then\n", &HashMap::new())
                .unwrap_err()
                .incomplete
        );
        assert!(
            parse(b"echo 'open", &HashMap::new())
                .unwrap_err()
                .incomplete
        );
        assert!(
            parse(b"cat <<END\nwrong\n", &HashMap::new())
                .unwrap_err()
                .incomplete
        );
        assert!(!parse(b"echo hi )", &HashMap::new()).unwrap_err().incomplete);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn interactive_input_waits_after_line_continuation() {
        let aliases = HashMap::new();
        assert!(
            parse_next(b"echo continued\\\n", &aliases)
                .unwrap_err()
                .incomplete
        );
        assert!(
            parse_next(b"echo continued \\\n", &aliases)
                .unwrap_err()
                .incomplete
        );
        assert!(parse(b"echo continued\\\n", &aliases).is_ok());
        let (commands, consumed) = parse_next(b"echo continued\\\nword\nnext", &aliases).unwrap();
        let Kind::Simple(words) = &commands[0].chain.first.commands[0].kind else {
            panic!("expected a simple command");
        };
        assert_eq!(words[1].0, b"continuedword");
        assert_eq!(consumed, b"echo continued\\\nword\n".len());
    }

    #[test]
    fn recognizes_operators_split_by_line_continuation() {
        let source = b"cat <\\\n<-EOF &\\\n& :\n\tbody\n\tEOF\n";
        let commands = parse(source, &HashMap::new()).unwrap();
        assert_eq!(commands[0].chain.rest.len(), 1);
        let redirect = &commands[0].chain.first.commands[0].redirs[0];
        assert!(matches!(&redirect.kind, RedirKind::Here { body, .. } if body == b"body\n"));
    }

    #[test]
    fn strips_heredoc_tabs_after_line_continuation() {
        // Issue 8 section 2.7.4: tabs after a joined newline are no longer leading.
        let source = b"cat <<-END\n\tone\\\n\ttwo\n\tEND\n";
        let commands = parse(source, &HashMap::new()).unwrap();
        let redirect = &commands[0].chain.first.commands[0].redirs[0];
        assert!(matches!(&redirect.kind, RedirKind::Here { body, .. } if body == b"one\ttwo\n"));
    }

    #[test]
    fn admits_compound_commands_before_reserved_closers() {
        for source in [
            &b"{ (:) }"[..],
            &b"if true; then (:) fi"[..],
            &b"case foo in x) { :; } esac"[..],
        ] {
            assert!(parse(source, &HashMap::new()).is_ok(), "{source:?}");
        }
    }

    #[test]
    fn requires_one_sequential_separator_before_do() {
        assert!(parse(b"for i\n; do :; done", &HashMap::new()).is_err());
        assert!(parse(b"for i;\n do :; done", &HashMap::new()).is_ok());
    }
}

#[cfg(test)]
mod quoting_regressions {
    use super::*;

    #[test]
    fn dollar_single_quotes_decode_control_and_nul_per_issue_eight() {
        assert_eq!(
            decode_dollar_single(br"'\cA\c\\\141\x62\t").unwrap(),
            b"'\x01\x1cab\t"
        );
        assert_eq!(decode_dollar_single(br"a\000suffix").unwrap(), b"a");
        assert!(decode_dollar_single(br"\x").is_err());
    }

    #[test]
    fn empty_alias_removes_command_word() {
        let aliases = HashMap::from([(b"empty".to_vec(), Vec::new())]);
        let (commands, consumed) = parse_next(b"empty\nnext", &aliases).unwrap();
        assert!(commands.is_empty());
        assert_eq!(consumed, b"empty\n".len());
        let commands = parse(b"empty :\n", &aliases).unwrap();
        let Kind::Simple(words) = &commands[0].chain.first.commands[0].kind else {
            panic!("expected a simple command");
        };
        assert_eq!(words[0].0, b":");
    }
}

#[cfg(test)]
mod heredoc_regressions {
    use super::*;

    #[test]
    fn unquoted_heredoc_delimiter_preserves_literal_expansion_construct() {
        let source = b"cat <<$(printf 'END')\n$x\n$(printf 'END')\n";
        let commands = parse(source, &HashMap::new()).unwrap();
        let redirect = &commands[0].chain.first.commands[0].redirs[0];
        assert!(
            matches!(&redirect.kind, RedirKind::Here { body, expand: true } if body == b"$x\n")
        );
    }
}

#[cfg(test)]
mod location_tests {
    use super::*;

    #[test]
    fn startup_locations_follow_increments_and_use_the_original_command() {
        let text = b"# profile\nf() {\n    some-command 'a b' >/dev/null\n}\n";
        let source = SourceLocation::new(text, Path::new("/tmp/profile.sh"));
        let aliases = HashMap::new();
        let (first, consumed) = parse_located(&source, 0, &aliases, true).unwrap();
        assert!(first.is_empty());
        let (commands, _) = parse_located(&source, consumed, &aliases, true).unwrap();
        let Kind::Function(_, function) = &commands[0].chain.first.commands[0].kind else {
            panic!("expected function");
        };
        let Kind::Group(body) = &function.kind else {
            panic!("expected group");
        };
        let site = body[0].chain.first.commands[0].site.as_ref().unwrap();
        assert_eq!(
            site.report("not found"),
            "/tmp/profile.sh:3: \"some-command 'a b' >/dev/null\": not found"
        );
        assert!(
            parse(text, &aliases).unwrap()[0].chain.first.commands[0]
                .site
                .is_none()
        );
    }

    #[test]
    fn startup_syntax_error_identifies_the_physical_line() {
        let source = SourceLocation::new(b":\n\nif; then\n", Path::new("/tmp/conf.sh"));
        let error = parse_located(&source, 2, &HashMap::new(), false).unwrap_err();
        assert_eq!(
            source.syntax_error(2, &error),
            "/tmp/conf.sh:3: \"if; then\": expected command, found ;"
        );
    }
}
