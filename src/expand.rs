use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;

use crate::{arith, shell::Shell, syntax};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Word,
    Quoted,
    Parameter,
    Here,
    Env,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tilde {
    None,
    Leading,
    Assignment,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PieceKind {
    Literal,
    Quoted,
    Expansion,
}

struct Piece {
    value: Vec<u8>,
    kind: PieceKind,
}

#[derive(Default)]
struct Field {
    pieces: Vec<Piece>,
}

impl Field {
    fn append(&mut self, bytes: &[u8], kind: PieceKind) {
        if bytes.is_empty() && kind != PieceKind::Quoted {
            return;
        }
        if kind != PieceKind::Expansion && !bytes.is_empty() {
            if let Some(piece) = self.pieces.last_mut().filter(|piece| piece.kind == kind) {
                piece.value.extend_from_slice(bytes);
                return;
            }
        }
        self.pieces.push(Piece {
            value: bytes.to_vec(),
            kind,
        });
    }

    fn append_owned(&mut self, mut value: Vec<u8>, kind: PieceKind) {
        if value.is_empty() && kind != PieceKind::Quoted {
            return;
        }
        if kind != PieceKind::Expansion && !value.is_empty() {
            if let Some(piece) = self.pieces.last_mut().filter(|piece| piece.kind == kind) {
                piece.value.append(&mut value);
                return;
            }
        }
        self.pieces.push(Piece { value, kind });
    }

    fn into_value(mut self) -> Vec<u8> {
        if self.pieces.len() == 1 {
            return self.pieces.pop().unwrap().value;
        }
        let mut output = Vec::with_capacity(self.pieces.iter().map(|p| p.value.len()).sum());
        for piece in self.pieces {
            output.extend(piece.value);
        }
        output
    }

    fn pattern(&self) -> Vec<u8> {
        let mut pattern = Vec::new();
        for piece in &self.pieces {
            for &byte in &piece.value {
                if byte == b'\\'
                    || piece.kind == PieceKind::Quoted
                        && matches!(byte, b'*' | b'?' | b'[' | b']' | b'-' | b'!' | b'^')
                {
                    pattern.push(b'\\');
                }
                pattern.push(byte);
            }
        }
        pattern
    }

    fn has_pattern(&self) -> bool {
        self.pieces.iter().any(|piece| {
            piece.kind != PieceKind::Quoted && piece.value.iter().any(|byte| b"*?[".contains(byte))
        })
    }
}

enum Sub {
    Scalar(Vec<u8>),
    Fields(Vec<Field>),
    Positional(Vec<Vec<u8>>),
}

pub fn words_into(
    shell: &mut Shell,
    source: &[u8],
    output: &mut Vec<Vec<u8>>,
) -> Result<(), String> {
    if source.is_empty() {
        return Ok(());
    }
    let noglob = shell.options.noglob;
    if literal(source, Tilde::Leading, !noglob) {
        output.push(source.to_vec());
        return Ok(());
    }
    if let Some((name, quoted)) = direct_name(source) {
        let value = named_value(shell, name)?;
        if quoted {
            output.push(value.to_vec());
            return Ok(());
        }
        if noglob || !contains_glob(value) {
            let fields = SplitUnquoted {
                input: value,
                delimiters: Delimiters::new(shell.ifs()),
            };
            for field in fields {
                output.push(field.to_vec());
            }
            return Ok(());
        }
    }

    let fields = scan(shell, source, Mode::Word, Tilde::Leading, true)?;
    let delimiters = Delimiters::new(shell.ifs());
    for field in fields {
        if field.pieces.is_empty() {
            continue;
        }
        if field.pieces.iter().any(|p| p.kind == PieceKind::Expansion) {
            for field in split(field, &delimiters) {
                expand_path(field, noglob, output);
            }
        } else {
            expand_path(field, noglob, output);
        }
    }
    Ok(())
}

pub fn scalar(shell: &mut Shell, source: &[u8]) -> Result<Vec<u8>, String> {
    scalar_with_tilde(shell, source, Tilde::Leading)
}

pub fn assignment_value(shell: &mut Shell, source: &[u8]) -> Result<Vec<u8>, String> {
    scalar_with_tilde(shell, source, Tilde::Assignment)
}

fn scalar_with_tilde(shell: &mut Shell, source: &[u8], tilde: Tilde) -> Result<Vec<u8>, String> {
    if literal(source, tilde, false) {
        return Ok(source.to_vec());
    }
    if let Some((name, _)) = direct_name(source) {
        return Ok(named_value(shell, name)?.to_vec());
    }
    let fields = scan(shell, source, Mode::Word, tilde, false)?;
    Ok(join_fields(fields, shell.ifs()))
}

pub fn here(shell: &mut Shell, source: &[u8]) -> Result<Vec<u8>, String> {
    if !source
        .iter()
        .any(|byte| matches!(byte, b'$' | b'\x60' | b'\\'))
    {
        return Ok(source.to_vec());
    }
    let fields = scan(shell, source, Mode::Here, Tilde::None, false)?;
    Ok(join_fields(fields, shell.ifs()))
}

/// Interpret the ENV pathname's parameter expansions in isolation from startup code.
pub fn env_path(shell: &mut Shell, source: &[u8]) -> Result<Vec<u8>, String> {
    let fields = scan(shell, source, Mode::Env, Tilde::None, false)?;
    Ok(join_fields(fields, shell.ifs()))
}

fn contains_glob(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

fn literal(source: &[u8], tilde: Tilde, glob: bool) -> bool {
    !source.starts_with(b"~")
        && !source.iter().any(|byte| {
            matches!(byte, b'\'' | b'"' | b'\\' | b'$' | b'\x60')
                || (glob && matches!(byte, b'*' | b'?' | b'['))
        })
        && (tilde != Tilde::Assignment || !source.windows(2).any(|pair| pair == b":~"))
}

fn direct_name(source: &[u8]) -> Option<(&[u8], bool)> {
    let (source, quoted) = match source
        .strip_prefix(b"\"")
        .and_then(|s| s.strip_suffix(b"\""))
    {
        Some(source) => (source, true),
        None => (source, false),
    };
    let name = source.strip_prefix(b"$")?;
    let name = name
        .strip_prefix(b"{")
        .and_then(|s| s.strip_suffix(b"}"))
        .unwrap_or(name);
    crate::shell::is_name(name).then_some((name, quoted))
}

fn named_value<'a>(shell: &'a Shell, name: &[u8]) -> Result<&'a [u8], String> {
    match shell.get(name) {
        Some(value) => Ok(value),
        None if shell.options.nounset => Err(format!(
            "{}: parameter is unset",
            String::from_utf8_lossy(name)
        )),
        None => Ok(b""),
    }
}

pub fn pattern(shell: &mut Shell, source: &[u8]) -> Result<Vec<u8>, String> {
    if literal(source, Tilde::Leading, false) {
        return Ok(source.to_vec());
    }
    let fields = scan(shell, source, Mode::Word, Tilde::Leading, false)?;
    let mut patterns = fields.iter();
    let mut result = patterns.next().map(Field::pattern).unwrap_or_default();
    for field in patterns {
        result.push(b' ');
        result.extend(field.pattern());
    }
    Ok(result)
}

pub fn matches(pattern: &[u8], value: &[u8]) -> bool {
    if plain_pattern(pattern) {
        return pattern == value;
    }
    let (Ok(pattern), Ok(value)) = (CString::new(pattern), CString::new(value)) else {
        return false;
    };
    match_cstr(&pattern, &value)
}

fn plain_pattern(pattern: &[u8]) -> bool {
    !pattern
        .iter()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b'\\' | 0))
}

fn match_cstr(pattern: &CStr, value: &CStr) -> bool {
    // Both strings are valid for the duration of this call.
    unsafe { libc::fnmatch(pattern.as_ptr(), value.as_ptr(), 0) == 0 }
}

fn pattern_cut(value: &[u8], pattern: Vec<u8>, prefix: bool, longest: bool) -> Option<usize> {
    let pattern = CString::new(pattern).ok()?;
    let mut candidate = Vec::with_capacity(value.len() + 1);
    candidate.extend_from_slice(value);
    candidate.push(0);
    let mut matches_cut = |cut| {
        if prefix {
            // Test a prefix in place, then restore the byte for the next cut.
            // CStr rejects candidates with embedded NUL bytes as before.
            let old = std::mem::replace(&mut candidate[cut], 0);
            let matched = CStr::from_bytes_with_nul(&candidate[..=cut])
                .is_ok_and(|text| match_cstr(&pattern, text));
            candidate[cut] = old;
            matched
        } else {
            CStr::from_bytes_with_nul(&candidate[cut..])
                .is_ok_and(|text| match_cstr(&pattern, text))
        }
    };
    let mut cuts = 0..=value.len();
    if prefix == longest {
        cuts.rev().find(|&cut| matches_cut(cut))
    } else {
        cuts.find(|&cut| matches_cut(cut))
    }
}

fn utf8_ifs(ifs: &[u8]) -> Option<&str> {
    // Match complete UTF-8 characters while preserving arbitrary input bytes.
    let text = std::str::from_utf8(ifs).ok()?;
    // nl_langinfo returns a borrowed pointer to a NUL-terminated static
    // string; locale initialization is single-threaded.
    let codeset = unsafe { CStr::from_ptr(libc::nl_langinfo(libc::CODESET)) };
    (codeset.to_bytes() == b"UTF-8").then_some(text)
}

enum Delimiters<'a> {
    Bytes(&'a [u8]),
    Utf8(Vec<&'a [u8]>),
}

impl<'a> Delimiters<'a> {
    fn new(ifs: &'a [u8]) -> Self {
        if !ifs.is_ascii() {
            if let Some(text) = utf8_ifs(ifs) {
                return Self::Utf8(
                    text.char_indices()
                        .map(|(start, ch)| &ifs[start..start + ch.len_utf8()])
                        .collect(),
                );
            }
        }
        Self::Bytes(ifs)
    }

    fn at(&self, input: &[u8]) -> Option<&'a [u8]> {
        match self {
            Self::Bytes(ifs) => {
                let first = input.first()?;
                let index = ifs.iter().position(|byte| byte == first)?;
                Some(&ifs[index..index + 1])
            }
            Self::Utf8(delimiters) => delimiters
                .iter()
                .copied()
                .find(|delimiter| input.starts_with(delimiter)),
        }
    }
}

fn whitespace_delimiter(delimiter: &[u8]) -> bool {
    matches!(delimiter, [b' ' | b'\t' | b'\n'])
}

/// A single unquoted parameter contributes byte slices until pathname expansion
/// is needed. A non-whitespace delimiter adjacent to whitespace ends the same
/// field, and leading non-whitespace delimiters can contribute empty fields.
struct SplitUnquoted<'a> {
    input: &'a [u8],
    delimiters: Delimiters<'a>,
}

impl<'a> SplitUnquoted<'a> {
    fn trim_whitespace(&mut self) {
        while let Some(delimiter) = self.delimiters.at(self.input) {
            if !whitespace_delimiter(delimiter) {
                break;
            }
            self.input = &self.input[delimiter.len()..];
        }
    }
}

impl<'a> Iterator for SplitUnquoted<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        self.trim_whitespace();
        if self.input.is_empty() {
            return None;
        }
        let end = (0..self.input.len())
            .find(|&i| self.delimiters.at(&self.input[i..]).is_some())
            .unwrap_or(self.input.len());
        let (field, rest) = self.input.split_at(end);
        self.input = rest;
        self.trim_whitespace();
        if let Some(delimiter) = self.delimiters.at(self.input) {
            self.input = &self.input[delimiter.len()..];
        }
        Some(field)
    }
}

fn first_ifs(ifs: &[u8]) -> &[u8] {
    let Some(first) = ifs.first() else {
        return b"";
    };
    let length = if first.is_ascii() {
        1
    } else {
        utf8_ifs(ifs)
            .and_then(|text| text.chars().next())
            .map_or(1, char::len_utf8)
    };
    &ifs[..length]
}

fn join_fields(mut fields: Vec<Field>, ifs: &[u8]) -> Vec<u8> {
    if fields.len() <= 1 {
        return fields.pop().map_or_else(Vec::new, Field::into_value);
    }
    let separator = first_ifs(ifs);
    let bytes = fields
        .iter()
        .flat_map(|field| &field.pieces)
        .map(|piece| piece.value.len())
        .sum::<usize>();
    let mut result = Vec::with_capacity(bytes + (fields.len() - 1) * separator.len());
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            result.extend_from_slice(separator);
        }
        for piece in field.pieces {
            result.extend(piece.value);
        }
    }
    result
}

fn piece_kind(mode: Mode, expanded: bool) -> PieceKind {
    if mode != Mode::Word {
        PieceKind::Quoted
    } else if expanded {
        PieceKind::Expansion
    } else {
        PieceKind::Literal
    }
}

fn append(fields: &mut [Field], value: &[u8], mode: Mode, expanded: bool) {
    fields
        .last_mut()
        .unwrap()
        .append(value, piece_kind(mode, expanded));
}

fn resolve_tilde(shell: &Shell, source: &[u8], context: Tilde) -> Option<(Vec<u8>, usize)> {
    let end = source
        .iter()
        .position(|&byte| byte == b'/' || context == Tilde::Assignment && byte == b':')
        .unwrap_or(source.len());
    let user = &source[1..end];
    let mut home = if user.is_empty() {
        shell.get(b"HOME")?.to_vec()
    } else if user
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(byte))
    {
        user_home(user)?
    } else {
        return None;
    };
    if source.get(end) == Some(&b'/') && home.last() == Some(&b'/') {
        home.pop();
    }
    Some((home, end))
}

fn scan(
    shell: &mut Shell,
    source: &[u8],
    mode: Mode,
    tilde: Tilde,
    split_fields: bool,
) -> Result<Vec<Field>, String> {
    let mut fields = vec![Field::default()];
    if mode == Mode::Parameter && source.is_empty() {
        append(&mut fields, b"", Mode::Quoted, false);
    }
    let mut i = 0;
    let mut tilde_start = tilde != Tilde::None;
    let mut double_quote_start = None;
    while i < source.len() {
        let b = source[i];
        let quote = if double_quote_start.is_some() {
            Mode::Quoted
        } else {
            mode
        };
        let check_tilde = std::mem::take(&mut tilde_start);
        if let Some((home, end)) = (check_tilde && b == b'~')
            .then(|| resolve_tilde(shell, &source[i..], tilde))
            .flatten()
        {
            fields
                .last_mut()
                .unwrap()
                .append_owned(home, PieceKind::Quoted);
            i += end;
            continue;
        }
        match b {
            b'\'' if quote == Mode::Word => {
                let end = source[i + 1..]
                    .iter()
                    .position(|&c| c == b'\'')
                    .ok_or("unterminated single quote")?
                    + i
                    + 1;
                append(&mut fields, &source[i + 1..end], Mode::Quoted, false);
                i = end + 1;
            }
            b'"' if matches!(mode, Mode::Word | Mode::Parameter) => {
                if let Some(start) = double_quote_start.take() {
                    // Preserve empty quoted fragments in their position.
                    if source[start..i].chunks(2).all(|pair| pair == b"\\\n") {
                        append(&mut fields, b"", Mode::Quoted, false);
                    }
                } else {
                    double_quote_start = Some(i + 1);
                }
                i += 1;
            }
            b'\\' if i + 1 < source.len() => {
                let next = source[i + 1];
                let special = match quote {
                    Mode::Word => true,
                    Mode::Quoted => b"$\x60\\\"\n".contains(&next),
                    Mode::Parameter => b"$\x60\\\"\n}".contains(&next),
                    Mode::Here => b"$\x60\\\n".contains(&next),
                    Mode::Env => false,
                };
                if special {
                    if next != b'\n' {
                        append(&mut fields, &[next], Mode::Quoted, false);
                    }
                    i += 2;
                } else {
                    append(&mut fields, &[b], quote, false);
                    i += 1;
                }
            }
            b'$' | b'\x60' => {
                let (sub, end) = substitute(shell, source, i, quote, split_fields)?;
                match sub {
                    Sub::Scalar(value) => fields
                        .last_mut()
                        .unwrap()
                        .append_owned(value, piece_kind(quote, true)),
                    Sub::Positional(values) => {
                        for (j, value) in values.into_iter().enumerate() {
                            if j > 0 {
                                fields.push(Field::default());
                            }
                            fields
                                .last_mut()
                                .unwrap()
                                .append_owned(value, piece_kind(quote, true));
                        }
                    }
                    Sub::Fields(values) => {
                        let mut values = values.into_iter();
                        if let Some(first) = values.next() {
                            fields.last_mut().unwrap().pieces.extend(first.pieces);
                            fields.extend(values);
                        }
                    }
                }
                i = end;
            }
            _ => {
                tilde_start = tilde == Tilde::Assignment && quote == Mode::Word && b == b':';
                let end = if tilde_start {
                    i + 1
                } else {
                    let tail = &source[i + 1..];
                    i + 1
                        + tail
                            .iter()
                            .position(|byte| {
                                matches!(byte, b'\'' | b'"' | b'\\' | b'$' | b'\x60')
                                    || tilde == Tilde::Assignment
                                        && quote == Mode::Word
                                        && *byte == b':'
                            })
                            .unwrap_or(tail.len())
                };
                append(&mut fields, &source[i..end], quote, false);
                i = end;
            }
        }
    }
    Ok(fields)
}

fn substitute(
    shell: &mut Shell,
    source: &[u8],
    i: usize,
    quote: Mode,
    split_fields: bool,
) -> Result<(Sub, usize), String> {
    if quote == Mode::Env && (source[i] == b'\x60' || source.get(i + 1) == Some(&b'(')) {
        let end = syntax::expansion_end(source, i, true)
            .ok()
            .flatten()
            .unwrap_or(i + 1);
        return Ok((Sub::Scalar(source[i..end].to_vec()), end));
    }
    if let Some(end) =
        syntax::expansion_end(source, i, quote != Mode::Word).map_err(|e| e.message)?
    {
        let raw = &source[i..end];
        let value = if raw[0] == b'\x60' {
            let mut program = Vec::new();
            let mut chars = raw[1..raw.len() - 1].iter().copied().peekable();
            while let Some(b) = chars.next() {
                if b == b'\\' && chars.peek().is_some_and(|c| b"\\$\x60".contains(c)) {
                    program.push(chars.next().unwrap());
                } else {
                    program.push(b);
                }
            }
            Sub::Scalar(shell.substitute(&program)?)
        } else if raw.starts_with(b"$'") {
            let mut f = Field::default();
            f.append(
                &syntax::decode_dollar_single(&raw[2..raw.len() - 1])?,
                PieceKind::Quoted,
            );
            Sub::Fields(vec![f])
        } else if raw.starts_with(b"$((") {
            let expr = scalar(shell, &raw[3..raw.len() - 2])?;
            Sub::Scalar(arith::evaluate(shell, &expr)?.to_string().into_bytes())
        } else if raw.starts_with(b"$(") {
            Sub::Scalar(shell.substitute(&raw[2..raw.len() - 1])?)
        } else {
            parameter(shell, &raw[2..raw.len() - 1], quote, split_fields)?
        };
        return Ok((value, end));
    }
    let count = name_len(&source[i + 1..], false);
    if count == 0 {
        return Ok((Sub::Scalar(vec![b'$']), i + 1));
    }
    let end = i + 1 + count;
    Ok((
        parameter(shell, &source[i + 1..end], quote, split_fields)?,
        end,
    ))
}

fn name_len(source: &[u8], braces: bool) -> usize {
    match source.first().copied() {
        Some(b'0'..=b'9') if braces => source.iter().take_while(|b| b.is_ascii_digit()).count(),
        Some(b'?' | b'$' | b'#' | b'@' | b'*' | b'!' | b'-' | b'0'..=b'9') => 1,
        Some(b'a'..=b'z' | b'A'..=b'Z' | b'_') => source
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count(),
        _ => 0,
    }
}

fn parameter(
    shell: &mut Shell,
    source: &[u8],
    quote: Mode,
    split_fields: bool,
) -> Result<Sub, String> {
    if source.len() > 1 && source[0] == b'#' && name_len(&source[1..], true) == source.len() - 1 {
        let name = &source[1..];
        let len = if matches!(name, b"@" | b"*") {
            shell.args.len()
        } else {
            match shell.parameter(name) {
                Some(value) => {
                    std::str::from_utf8(&value).map_or(value.len(), |s| s.chars().count())
                }
                None if shell.options.nounset => {
                    return Err(format!(
                        "{}: parameter is unset",
                        String::from_utf8_lossy(name)
                    ));
                }
                None => 0,
            }
        };
        return Ok(Sub::Scalar(len.to_string().into_bytes()));
    }
    let n = name_len(source, true);
    if n == 0 {
        return Err("invalid parameter expansion".into());
    }
    let (name, operator) = source.split_at(n);
    if operator.is_empty() && (name == b"@" || name == b"*" && quote == Mode::Word && split_fields)
    {
        return Ok(Sub::Positional(shell.args.clone()));
    }
    if operator.is_empty() && name == b"*" {
        return Ok(Sub::Scalar(shell.args.join(first_ifs(shell.ifs()))));
    }
    let value = shell.parameter(name);
    if operator.is_empty() {
        return match value {
            Some(value) => Ok(Sub::Scalar(value)),
            None if shell.options.nounset => Err(format!(
                "{}: parameter is unset",
                String::from_utf8_lossy(name)
            )),
            None => Ok(Sub::Scalar(Vec::new())),
        };
    }
    if let [b'#' | b'%', ..] = operator {
        let prefix = operator[0] == b'#';
        let longest = operator.get(1) == Some(&operator[0]);
        let value = value.unwrap_or_default();
        let pattern_word = &operator[1 + usize::from(longest)..];
        let pattern = if quote == Mode::Env {
            env_path(shell, pattern_word)?
        } else {
            pattern(shell, pattern_word)?
        };
        if plain_pattern(&pattern) {
            let remainder = if prefix {
                value.strip_prefix(pattern.as_slice())
            } else {
                value.strip_suffix(pattern.as_slice())
            };
            return Ok(Sub::Scalar(match remainder {
                Some(bytes) => bytes.to_vec(),
                None => value,
            }));
        }
        if let Some(cut) = pattern_cut(&value, pattern, prefix, longest) {
            let remainder = if prefix { &value[cut..] } else { &value[..cut] };
            return Ok(Sub::Scalar(remainder.to_vec()));
        }
        return Ok(Sub::Scalar(value));
    }
    let test_empty = operator.starts_with(b":");
    let Some((&kind, word)) = operator[usize::from(test_empty)..].split_first() else {
        return Err("invalid parameter operator".into());
    };
    let missing = value.as_ref().is_none_or(|v| test_empty && v.is_empty());
    let (word_mode, word_tilde) = match quote {
        Mode::Word => (Mode::Word, Tilde::Leading),
        Mode::Env => (Mode::Env, Tilde::None),
        _ => (Mode::Parameter, Tilde::None),
    };
    match kind {
        b'-' | b'+' => {
            if missing == (kind == b'-') {
                let mut fields = scan(shell, word, word_mode, word_tilde, split_fields)?;
                for field in &mut fields {
                    for piece in &mut field.pieces {
                        if piece.kind == PieceKind::Literal {
                            piece.kind = PieceKind::Expansion;
                        }
                    }
                }
                Ok(Sub::Fields(fields))
            } else if kind == b'-' {
                Ok(Sub::Scalar(value.unwrap_or_default()))
            } else {
                Ok(Sub::Scalar(Vec::new()))
            }
        }
        b'=' if missing => {
            let fields = scan(shell, word, word_mode, word_tilde, false)?;
            let result = join_fields(fields, shell.ifs());
            shell.assign(name, result.clone(), false)?;
            Ok(Sub::Scalar(result))
        }
        b'?' if missing => {
            let message = join_fields(
                scan(shell, word, word_mode, word_tilde, false)?,
                shell.ifs(),
            );
            let message = if message.is_empty() {
                "parameter is unset or null".into()
            } else {
                String::from_utf8_lossy(&message)
            };
            Err(format!("{}: {message}", String::from_utf8_lossy(name)))
        }
        b'=' | b'?' => Ok(Sub::Scalar(value.unwrap_or_default())),
        _ => Err("invalid parameter operator".into()),
    }
}

fn split(field: Field, delimiters: &Delimiters<'_>) -> Vec<Field> {
    let mut result = Vec::new();
    let mut next = Field::default();
    let mut pending_whitespace = false;
    for piece in field.pieces {
        if piece.kind != PieceKind::Expansion {
            if pending_whitespace && !next.pieces.is_empty() {
                result.push(std::mem::take(&mut next));
            }
            pending_whitespace = false;
            next.append_owned(piece.value, piece.kind);
            continue;
        }
        let mut input = piece.value.as_slice();
        while !input.is_empty() {
            if let Some(delimiter) = delimiters.at(input) {
                input = &input[delimiter.len()..];
                if whitespace_delimiter(delimiter) {
                    pending_whitespace = true;
                } else {
                    result.push(std::mem::take(&mut next));
                    pending_whitespace = false;
                }
            } else {
                if pending_whitespace && !next.pieces.is_empty() {
                    result.push(std::mem::take(&mut next));
                }
                pending_whitespace = false;
                let end = (1..input.len())
                    .find(|&i| delimiters.at(&input[i..]).is_some())
                    .unwrap_or(input.len());
                next.append(&input[..end], PieceKind::Literal);
                input = &input[end..];
            }
        }
    }
    if !next.pieces.is_empty() {
        result.push(next);
    }
    result
}

fn expand_path(field: Field, noglob: bool, output: &mut Vec<Vec<u8>>) {
    if !noglob && field.has_pattern() {
        if let Some(matches) = glob(field.pattern()) {
            output.extend(matches);
            return;
        }
    }
    output.push(field.into_value());
}

fn glob(pattern: Vec<u8>) -> Option<Vec<Vec<u8>>> {
    let pattern = CString::new(pattern).ok()?;
    // glob initializes glob_t; on return globfree releases any allocated entries.
    unsafe {
        let mut found = std::mem::zeroed::<libc::glob_t>();
        let status = libc::glob(pattern.as_ptr(), 0, None, &mut found);
        let result = if status == 0 && !found.gl_pathv.is_null() {
            Some(
                std::slice::from_raw_parts(found.gl_pathv, found.gl_pathc)
                    .iter()
                    .map(|p| CStr::from_ptr(*p).to_bytes().to_vec())
                    .collect(),
            )
        } else {
            None
        };
        libc::globfree(&mut found);
        result
    }
}

fn user_home(name: &[u8]) -> Option<Vec<u8>> {
    let name = CString::new(name).ok()?;
    let mut buffer = vec![0u8; 16_384];
    // All returned passwd strings are copied before dropping the backing buffer.
    unsafe {
        let mut passwd = std::mem::zeroed::<libc::passwd>();
        let mut result = std::ptr::null_mut();
        let code = libc::getpwnam_r(
            name.as_ptr(),
            &mut passwd,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        );
        if code != 0 || result.is_null() || passwd.pw_dir.is_null() {
            return None;
        }
        Some(CStr::from_ptr(passwd.pw_dir).to_bytes().to_vec())
    }
}

pub fn cwd() -> Vec<u8> {
    std::env::current_dir().map_or_else(|_| Vec::new(), |p| p.as_os_str().as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Options;

    #[test]
    fn wildcard_removal_keeps_shortest_longest_and_embedded_nul_rules() {
        assert_eq!(pattern_cut(b"a/b/c", b"*/".to_vec(), true, false), Some(2));
        assert_eq!(pattern_cut(b"a/b/c", b"*/".to_vec(), true, true), Some(4));
        assert_eq!(pattern_cut(b"a/b/c", b"/*".to_vec(), false, false), Some(3));
        assert_eq!(pattern_cut(b"a/b/c", b"/*".to_vec(), false, true), Some(1));
        assert_eq!(pattern_cut(b"a\0b", b"*".to_vec(), true, true), Some(1));
        assert_eq!(pattern_cut(b"a\0b", b"*".to_vec(), false, true), Some(2));
        assert!(matches(b"\\*", b"*"));
        assert!(!matches(b"literal\0", b"literal\0"));
    }

    #[test]
    fn sliced_parameter_splitting_matches_piece_splitting() {
        let delimiters = Delimiters::Bytes(b" :\n\t");
        for input in [
            b"".as_slice(),
            b"\t  ",
            b"a : b::",
            b" :a\t\n  :b :\t:c",
            b"\xff\na:\xff \t",
        ] {
            let mut field = Field::default();
            field.append(input, PieceKind::Expansion);
            let general = split(field, &delimiters)
                .into_iter()
                .map(Field::into_value)
                .collect::<Vec<_>>();
            let sliced = SplitUnquoted {
                input,
                delimiters: Delimiters::Bytes(b" :\n\t"),
            }
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
            assert_eq!(sliced, general, "input: {input:?}");
        }

        let words = SplitUnquoted {
            input: " ≤ x≤≤y ".as_bytes(),
            delimiters: Delimiters::Utf8(vec!["≤".as_bytes(), b" "]),
        }
        .collect::<Vec<_>>();
        assert_eq!(words, [b"".as_slice(), b"x", b"", b"y"]);
    }

    #[test]
    fn env_path_expands_parameters_without_interpreting_other_syntax() {
        let mut shell = Shell::new(
            b"nsh".to_vec(),
            Vec::new(),
            Options::default(),
            false,
            false,
            false,
        );
        shell
            .assign(b"HOME", b"/tmp/home with spaces".to_vec(), false)
            .unwrap();
        let input = [
            b"$HOME/".as_slice(),
            b"$",
            b"{unset:-$(printf invalid)}/",
            b"'literal'/$((1+1))/\x60printf invalid\x60",
        ]
        .concat();
        let result = env_path(&mut shell, &input).unwrap();
        assert_eq!(
            result,
            b"/tmp/home with spaces/$(printf invalid)/'literal'/$((1+1))/\x60printf invalid\x60",
        );
    }
}
