//! POSIX.1-2024 printf with byte-preserving operands and bounded output buffers.
use std::borrow::Cow;
use std::ffi::CString;
use std::io;

use crate::shell::{Flow, Shell, diagnostic, output, show};
use crate::sys;

const LEFT: u8 = 1;
const PLUS: u8 = 2;
const SPACE: u8 = 4;
const ALTERNATE: u8 = 8;
const ZERO: u8 = 16;

pub fn run(shell: &Shell, args: &[Vec<u8>]) -> Flow {
    let args = if args.first().is_some_and(|arg| arg == b"--") {
        &args[1..]
    } else {
        if args.len() == 1 && args[0] == b"--help" {
            return output(b"usage: printf [--] format [argument ...]\n");
        }
        if args.len() == 1 && args[0] == b"--version" {
            return output(concat!("printf (NSH) ", env!("CARGO_PKG_VERSION"), "\n").as_bytes());
        }
        args
    };
    let Some((format, arguments)) = args.split_first() else {
        return shell.error(1, "printf: missing format operand", false);
    };
    if let [value] = arguments {
        match format.as_slice() {
            b"%s" => return written(sys::write(1, value)),
            b"%s\n" | b"%s\\n" if value.len() < 256 => {
                let mut line = [0; 256];
                line[..value.len()].copy_from_slice(value);
                line[value.len()] = b'\n';
                return written(sys::write(1, &line[..=value.len()]));
            }
            _ => {}
        }
    }
    let mut writer = Writer::new();
    let mut status = 0;
    let result = render(shell, &mut writer, format, arguments, &mut status);
    if let Err(error) = writer.flush() {
        return written(Err(error));
    }
    match result {
        Ok(()) => Flow::Status(status),
        Err(RenderError::Invalid(message)) => shell.error(1, &format!("printf: {message}"), false),
        Err(RenderError::Write(error)) => written(Err(error)),
    }
}

fn written(result: io::Result<()>) -> Flow {
    match result {
        Ok(()) => Flow::Status(0),
        Err(error) => {
            diagnostic(&format!("printf: write: {error}"));
            Flow::Status(1)
        }
    }
}

enum RenderError {
    Write(io::Error),
    Invalid(&'static str),
}

impl From<io::Error> for RenderError {
    fn from(value: io::Error) -> Self {
        Self::Write(value)
    }
}

struct Writer {
    buffer: [u8; 512],
    len: usize,
}

impl Writer {
    fn new() -> Self {
        Self {
            buffer: [0; 512],
            len: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > self.buffer.len() - self.len {
            self.flush()?;
            if bytes.len() >= self.buffer.len() {
                return sys::write(1, bytes);
            }
        }
        let end = self.len + bytes.len();
        self.buffer[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        let written = sys::write(1, &self.buffer[..self.len]);
        self.len = 0;
        written
    }

    fn pad(&mut self, byte: u8, mut length: usize) -> io::Result<()> {
        let chunk = if byte == b'0' {
            &[b'0'; 512]
        } else {
            &[b' '; 512]
        };
        while length > chunk.len() {
            self.write(chunk)?;
            length -= chunk.len();
        }
        self.write(&chunk[..length])
    }
}

struct Arguments<'a> {
    values: &'a [Vec<u8>],
    next: usize,
    consumed: usize,
}

impl<'a> Arguments<'a> {
    fn get(&mut self, position: Option<usize>) -> Option<&'a [u8]> {
        let index = position.unwrap_or_else(|| {
            let next = self.next;
            self.next += 1;
            next
        });
        self.consumed = self.consumed.max(index + 1);
        self.values.get(index).map(Vec::as_slice)
    }
}

struct Conversion {
    kind: u8,
    flags: u8,
    width: usize,
    precision: Option<usize>,
    position: Option<usize>,
}

fn render(
    shell: &Shell,
    writer: &mut Writer,
    format: &[u8],
    mut values: &[Vec<u8>],
    status: &mut i32,
) -> Result<(), RenderError> {
    loop {
        let mut left = format;
        let mut args = Arguments {
            values,
            next: 0,
            consumed: 0,
        };
        while !left.is_empty() {
            let literal = left
                .iter()
                .position(|byte| matches!(byte, b'%' | b'\\'))
                .unwrap_or(left.len());
            writer.write(&left[..literal])?;
            left = &left[literal..];
            match left.first() {
                None => break,
                Some(b'\\') => match escape(shell, left, false)? {
                    Escaped::Stop => return Ok(()),
                    Escaped::Bytes {
                        bytes,
                        len,
                        consumed,
                    } => {
                        writer.write(&bytes[..len])?;
                        left = &left[consumed..];
                    }
                },
                Some(b'%') if left.get(1) == Some(&b'%') => {
                    writer.write(b"%")?;
                    left = &left[2..];
                }
                Some(b'%') => {
                    left = &left[1..];
                    let spec = conversion(shell, &mut left, &mut args, status)?;
                    let arg = args.get(spec.position);
                    match spec.kind {
                        b's' => text(writer, &spec, arg.unwrap_or_default(), false)?,
                        b'c' => text(
                            writer,
                            &spec,
                            &[arg.and_then(|v| v.first()).copied().unwrap_or(0)],
                            false,
                        )?,
                        b'b' => {
                            let (bytes, stop) = escaped_operand(shell, arg.unwrap_or_default())?;
                            text(writer, &spec, &bytes, stop)?;
                            if stop {
                                return Ok(());
                            }
                        }
                        b'q' => text(writer, &spec, &quote(shell, arg.unwrap_or_default()), false)?,
                        b'd' | b'i' | b'o' | b'u' | b'x' | b'X' => {
                            render_integer(shell, writer, &spec, arg, status)?;
                        }
                        b'a' | b'A' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                            render_float(shell, writer, &spec, arg, status)?;
                        }
                        _ => unreachable!("checked format conversion"),
                    }
                }
                _ => unreachable!("literal prefix already consumed"),
            }
        }
        if args.consumed == 0 && !values.is_empty() {
            diagnostic("printf: ignoring excess arguments");
        }
        if args.consumed == 0 || args.consumed >= values.len() {
            return Ok(());
        }
        values = &values[args.consumed..];
    }
}

fn conversion(
    shell: &Shell,
    input: &mut &[u8],
    args: &mut Arguments<'_>,
    status: &mut i32,
) -> Result<Conversion, RenderError> {
    let position = index(input)?;
    let mut flags = 0;
    while let Some((&first, rest)) = input.split_first() {
        flags |= match first {
            b'-' => LEFT,
            b'+' => PLUS,
            b' ' => SPACE,
            b'#' => ALTERNATE,
            b'0' => ZERO,
            _ => break,
        };
        *input = rest;
    }
    let mut width = dimension(shell, input, args, status)?.unwrap_or_default();
    if width < 0 {
        flags |= LEFT;
        width = width.saturating_abs();
    }
    let precision = if let Some(tail) = input.strip_prefix(b".") {
        *input = tail;
        let value = dimension(shell, input, args, status)?.unwrap_or_default();
        usize::try_from(value).ok()
    } else {
        None
    };
    // Accept integer/float length modifiers; operands still use shell-wide integer and double representations.
    while input.first().is_some_and(|b| b"hlLjzt".contains(b)) {
        *input = &input[1..];
    }
    let Some((&kind, rest)) = input.split_first() else {
        return Err(RenderError::Invalid("incomplete conversion"));
    };
    if !b"bcsqdiouxXaAeEfFgG".contains(&kind) {
        return Err(RenderError::Invalid("invalid conversion"));
    }
    *input = rest;
    Ok(Conversion {
        kind,
        flags,
        width: width as usize,
        precision,
        position,
    })
}

fn digit_count(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

fn decimal(bytes: &[u8]) -> Result<i32, RenderError> {
    let mut number = 0i32;
    for digit in bytes {
        number = number
            .checked_mul(10)
            .and_then(|number| number.checked_add(i32::from(*digit - b'0')))
            .ok_or(RenderError::Invalid(
                "field size or argument index is out of range",
            ))?;
    }
    Ok(number)
}

fn index(input: &mut &[u8]) -> Result<Option<usize>, RenderError> {
    let digits = digit_count(input);
    if digits == 0 || input.get(digits) != Some(&b'$') {
        return Ok(None);
    }
    let position = decimal(&input[..digits])?;
    if position == 0 {
        return Err(RenderError::Invalid("argument indices start at one"));
    }
    *input = &input[digits + 1..];
    Ok(Some(position as usize - 1))
}

fn dimension(
    shell: &Shell,
    input: &mut &[u8],
    args: &mut Arguments<'_>,
    status: &mut i32,
) -> Result<Option<i32>, RenderError> {
    if let Some(tail) = input.strip_prefix(b"*") {
        *input = tail;
        let position = index(input)?;
        let value = integer(shell, args.get(position), true, status) as i64;
        return i32::try_from(value)
            .map(Some)
            .map_err(|_| RenderError::Invalid("field width or precision is out of range"));
    }
    let digits = digit_count(input);
    let value = (digits != 0)
        .then(|| decimal(&input[..digits]))
        .transpose()?;
    *input = &input[digits..];
    Ok(value)
}

fn text(writer: &mut Writer, spec: &Conversion, bytes: &[u8], stop: bool) -> io::Result<()> {
    let length = if spec.kind == b'c' {
        bytes.len()
    } else {
        spec.precision.unwrap_or(bytes.len()).min(bytes.len())
    };
    let pad = if stop {
        0
    } else {
        spec.width.saturating_sub(length)
    };
    if spec.flags & LEFT == 0 {
        writer.pad(b' ', pad)?;
    }
    writer.write(&bytes[..length])?;
    if spec.flags & LEFT != 0 {
        writer.pad(b' ', pad)?;
    }
    Ok(())
}

fn render_integer(
    shell: &Shell,
    writer: &mut Writer,
    spec: &Conversion,
    arg: Option<&[u8]>,
    status: &mut i32,
) -> io::Result<()> {
    let signed = matches!(spec.kind, b'd' | b'i');
    let value = integer(shell, arg, signed, status);
    let (mut magnitude, prefix) = if signed {
        let value = value as i64;
        let sign: &[u8] = if value < 0 {
            b"-"
        } else if spec.flags & PLUS != 0 {
            b"+"
        } else if spec.flags & SPACE != 0 {
            b" "
        } else {
            b""
        };
        (value.unsigned_abs(), sign)
    } else {
        let prefix: &[u8] = if value != 0 && spec.flags & ALTERNATE != 0 {
            match spec.kind {
                b'x' => b"0x",
                b'X' => b"0X",
                _ => b"",
            }
        } else {
            b""
        };
        (value, prefix)
    };
    let base = match spec.kind {
        b'o' => 8,
        b'x' | b'X' => 16,
        _ => 10,
    };
    let alphabet = if spec.kind == b'X' {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut buffer = [0; 24];
    let mut offset = buffer.len();
    if magnitude != 0 || spec.precision != Some(0) {
        loop {
            offset -= 1;
            buffer[offset] = alphabet[(magnitude % base) as usize];
            magnitude /= base;
            if magnitude == 0 {
                break;
            }
        }
    }
    let digits = &buffer[offset..];
    let mut minimum = spec.precision.unwrap_or_default();
    if spec.kind == b'o' && spec.flags & ALTERNATE != 0 && digits.first() != Some(&b'0') {
        minimum = minimum.max(digits.len() + 1);
    }
    let precision_pad = minimum.saturating_sub(digits.len());
    let width_pad = spec
        .width
        .saturating_sub(prefix.len() + precision_pad + digits.len());
    let left = spec.flags & LEFT != 0;
    let zeros = !left && spec.flags & ZERO != 0 && spec.precision.is_none();
    if !left && !zeros {
        writer.pad(b' ', width_pad)?;
    }
    writer.write(prefix)?;
    writer.pad(b'0', precision_pad + if zeros { width_pad } else { 0 })?;
    writer.write(digits)?;
    if left {
        writer.pad(b' ', width_pad)?;
    }
    Ok(())
}

fn integer(shell: &Shell, arg: Option<&[u8]>, signed: bool, status: &mut i32) -> u64 {
    let Some(arg) = arg else { return 0 };
    if let Some(character) = quoted_number(shell, arg) {
        return u64::from(character);
    }
    let arg_trimmed = arg.trim_ascii_start();
    let (negative, digits) = if let Some(rest) = arg_trimmed.strip_prefix(b"-") {
        (true, rest)
    } else {
        (false, arg_trimmed.strip_prefix(b"+").unwrap_or(arg_trimmed))
    };
    let (base, digits) = radix(digits);
    let limit = if signed {
        i64::MAX as u64 + u64::from(negative)
    } else {
        u64::MAX
    };
    let (mut value, mut count, mut overflow) = (0u64, 0, false);
    for digit in digits.iter().map(|&digit| u64::from(hex_digit(digit))) {
        if digit >= base {
            break;
        }
        count += 1;
        if let Some(next) = value
            .checked_mul(base)
            .and_then(|value| value.checked_add(digit))
            .filter(|&value| value <= limit)
        {
            value = next;
        } else {
            overflow = true;
            value = limit;
        }
    }
    if overflow || count == 0 || count < digits.len() {
        numeric_error(arg, status);
    }
    if negative && (!overflow || signed) {
        value.wrapping_neg()
    } else {
        value
    }
}

fn radix(digits: &[u8]) -> (u64, &[u8]) {
    if digits.first() != Some(&b'0') {
        return (10, digits);
    }
    for (letters, base) in [(b"xX".as_slice(), 16), (b"bB".as_slice(), 2)] {
        if let [b'0', marker, following, ..] = digits {
            if letters.contains(marker) && u64::from(hex_digit(*following)) < base {
                return (base, &digits[2..]);
            }
        }
    }
    (8, digits)
}

fn quoted_number(shell: &Shell, arg: &[u8]) -> Option<u32> {
    let [b'\'' | b'"', rest @ ..] = arg else {
        return None;
    };
    let Some(&first) = rest.first() else {
        return Some(0);
    };
    if first.is_ascii() || !utf8_locale(shell) {
        return Some(u32::from(first));
    }
    Some(
        std::str::from_utf8(rest)
            .ok()
            .and_then(|text| text.chars().next())
            .map_or(u32::from(first), u32::from),
    )
}

fn numeric_error(arg: &[u8], status: &mut i32) {
    diagnostic(&format!(
        "printf: {:?}: invalid or out-of-range numeric operand",
        show(arg)
    ));
    *status = 1;
}

fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => u8::MAX,
    }
}

enum Escaped {
    Stop,
    Bytes {
        bytes: [u8; 10],
        len: usize,
        consumed: usize,
    },
}

fn escape(shell: &Shell, input: &[u8], operand: bool) -> Result<Escaped, RenderError> {
    let mut bytes = [0; 10];
    let Some(&letter) = input.get(1) else {
        bytes[0] = b'\\';
        return Ok(Escaped::Bytes {
            bytes,
            len: 1,
            consumed: 1,
        });
    };
    let named = match letter {
        b'a' => Some(0x07),
        b'b' => Some(0x08),
        b'e' | b'E' => Some(0x1b),
        b'f' => Some(0x0c),
        b'n' => Some(b'\n'),
        b'r' => Some(b'\r'),
        b't' => Some(b'\t'),
        b'v' => Some(0x0b),
        b'\\' | b'"' | b'\'' => Some(letter),
        b'c' => return Ok(Escaped::Stop),
        _ => None,
    };
    if let Some(byte) = named {
        bytes[0] = byte;
        return Ok(Escaped::Bytes {
            bytes,
            len: 1,
            consumed: 2,
        });
    }
    let (start, count, base) = match letter {
        b'0' if operand => (2, 3, 8),
        b'0'..=b'7' => (1, 3, 8),
        b'x' => (2, 2, 16),
        b'u' => (2, 4, 16),
        b'U' => (2, 8, 16),
        _ => {
            bytes[..2].copy_from_slice(&input[..2]);
            return Ok(Escaped::Bytes {
                bytes,
                len: 2,
                consumed: 2,
            });
        }
    };
    let mut value = 0u32;
    let mut used = 0;
    for digit in input[start..]
        .iter()
        .take(count)
        .map(|&digit| u32::from(hex_digit(digit)))
    {
        if digit >= base {
            break;
        }
        value = value * base + digit;
        used += 1;
    }
    let consumed = start + used;
    if matches!(letter, b'u' | b'U') {
        if used < count {
            return Err(RenderError::Invalid("incomplete Unicode escape"));
        }
        let character =
            char::from_u32(value).ok_or(RenderError::Invalid("invalid Unicode character"))?;
        let len = if character.is_ascii() || utf8_locale(shell) {
            character.encode_utf8(&mut bytes).len()
        } else {
            let escaped = if letter == b'u' {
                format!("\\u{value:04X}")
            } else {
                format!("\\U{value:08X}")
            };
            bytes[..escaped.len()].copy_from_slice(escaped.as_bytes());
            escaped.len()
        };
        return Ok(Escaped::Bytes {
            bytes,
            len,
            consumed,
        });
    }
    if letter == b'x' && used == 0 {
        return Err(RenderError::Invalid("missing hexadecimal escape digits"));
    }
    bytes[0] = value as u8;
    Ok(Escaped::Bytes {
        bytes,
        len: 1,
        consumed,
    })
}

fn escaped_operand<'a>(
    shell: &Shell,
    input: &'a [u8],
) -> Result<(Cow<'a, [u8]>, bool), RenderError> {
    let Some(start) = input.iter().position(|&byte| byte == b'\\') else {
        return Ok((Cow::Borrowed(input), false));
    };
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(&input[..start]);
    let mut rest = &input[start..];
    loop {
        match escape(shell, rest, true)? {
            Escaped::Stop => return Ok((Cow::Owned(out), true)),
            Escaped::Bytes {
                bytes,
                len,
                consumed,
            } => {
                out.extend_from_slice(&bytes[..len]);
                rest = &rest[consumed..];
            }
        }
        let Some(start) = rest.iter().position(|&byte| byte == b'\\') else {
            out.extend_from_slice(rest);
            return Ok((Cow::Owned(out), false));
        };
        out.extend_from_slice(&rest[..start]);
        rest = &rest[start..];
    }
}

fn quote<'a>(shell: &Shell, bytes: &'a [u8]) -> Cow<'a, [u8]> {
    if !bytes.is_empty()
        && bytes
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || b"_@%+,:./=-".contains(&byte))
    {
        return Cow::Borrowed(bytes);
    }
    let print_unicode =
        !bytes.is_ascii() && utf8_locale(shell) && std::str::from_utf8(bytes).is_ok();
    let raw = bytes
        .iter()
        .any(|&byte| byte.is_ascii_control() || byte >= 0x80 && !print_unicode);
    let mut result = Vec::with_capacity(bytes.len() + 3);
    result.extend_from_slice(if raw { b"$'" } else { b"'" });
    for &byte in bytes {
        if raw {
            match byte {
                b'\'' | b'\\' => result.extend_from_slice(&[b'\\', byte]),
                0..=31 | 127..=255 => {
                    result.extend_from_slice(&[
                        b'\\',
                        b'0' + (byte >> 6),
                        b'0' + (byte >> 3 & 7),
                        b'0' + (byte & 7),
                    ]);
                }
                _ => result.push(byte),
            }
        } else if byte == b'\'' {
            result.extend_from_slice(b"'\\''");
        } else {
            result.push(byte);
        }
    }
    result.push(b'\'');
    Cow::Owned(result)
}

fn locale_setting<'a>(shell: &'a Shell, category: &[u8]) -> &'a [u8] {
    [b"LC_ALL".as_slice(), category, b"LANG"]
        .into_iter()
        .find_map(|key| {
            let variable = shell.vars.get(key)?;
            variable
                .exported
                .then_some(variable.value.as_deref()?)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or(b"C")
}

fn utf8_locale(shell: &Shell) -> bool {
    let locale = locale_setting(shell, b"LC_CTYPE");
    locale.windows(4).any(|w| w.eq_ignore_ascii_case(b"utf8"))
        || locale.windows(5).any(|w| w.eq_ignore_ascii_case(b"utf-8"))
}

struct NumericLocale {
    previous: libc::locale_t,
    owned: libc::locale_t,
}

impl NumericLocale {
    fn for_shell(shell: &Shell) -> Result<Self, RenderError> {
        let configured = CString::new(locale_setting(shell, b"LC_NUMERIC")).ok();
        // newlocale and uselocale own no shell data. The checked C string lives
        // through each call and the returned locale remains valid until Drop.
        unsafe {
            let mut owned = configured.map_or(std::ptr::null_mut(), |configured| {
                libc::newlocale(
                    libc::LC_NUMERIC_MASK,
                    configured.as_ptr(),
                    std::ptr::null_mut(),
                )
            });
            if owned.is_null() {
                owned = libc::newlocale(libc::LC_NUMERIC_MASK, c"C".as_ptr(), std::ptr::null_mut());
            }
            if owned.is_null() {
                return Err(RenderError::Invalid("cannot create numeric locale"));
            }
            let previous = libc::uselocale(owned);
            if previous.is_null() {
                libc::freelocale(owned);
                return Err(RenderError::Invalid("cannot select numeric locale"));
            }
            Ok(Self { previous, owned })
        }
    }
}

impl Drop for NumericLocale {
    fn drop(&mut self) {
        // Restoring the prior thread locale precedes releasing the owned locale.
        unsafe {
            libc::uselocale(self.previous);
            libc::freelocale(self.owned);
        }
    }
}

fn float_value(shell: &Shell, arg: Option<&[u8]>, status: &mut i32) -> f64 {
    let Some(arg) = arg else { return 0.0 };
    if let Some(character) = quoted_number(shell, arg) {
        return f64::from(character);
    }
    let Ok(value) = CString::new(arg) else {
        numeric_error(arg, status);
        return 0.0;
    };
    let mut end = std::ptr::null_mut();
    // The C string and writable end pointer live through strtod; errno is
    // thread-local and restored before the shell sees it.
    let (number, bad) = unsafe {
        let errno = libc::__errno_location();
        let previous = *errno;
        *errno = 0;
        let number = libc::strtod(value.as_ptr(), &mut end);
        let bad = *errno == libc::ERANGE
            || arg.is_empty()
            || end.cast_const() != value.as_ptr().wrapping_add(arg.len());
        *errno = previous;
        (number, bad)
    };
    if bad {
        numeric_error(arg, status);
    }
    number
}

fn render_float(
    shell: &Shell,
    writer: &mut Writer,
    spec: &Conversion,
    arg: Option<&[u8]>,
    status: &mut i32,
) -> Result<(), RenderError> {
    let _locale = NumericLocale::for_shell(shell)?;
    let value = float_value(shell, arg, status);

    // A finite binary64 has fewer than 1400 exact decimal digits. Rendering a
    // bounded precision lets us stream any additional zeroes requested by
    // the caller without allocating a buffer proportional to field size.
    const EXACT: usize = 1400;
    let precision = spec.precision.map(|precision| precision.min(EXACT) as i32);
    let mut format = [0; 7];
    format[0] = b'%';
    let mut count = 1;
    if spec.flags & (PLUS | SPACE) != 0 {
        format[count] = if spec.flags & PLUS != 0 { b'+' } else { b' ' };
        count += 1;
    }
    if spec.flags & ALTERNATE != 0 {
        format[count] = b'#';
        count += 1;
    }
    if precision.is_some() {
        format[count..count + 2].copy_from_slice(b".*");
        count += 2;
    }
    format[count] = spec.kind;
    // The terminator is already zero. The only permitted conversion consumes
    // exactly one c_double, with an optional c_int for dynamic precision.
    let mut bytes = [0u8; 2048];
    let len = unsafe {
        if let Some(precision) = precision {
            libc::snprintf(
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                format.as_ptr().cast(),
                precision,
                value,
            )
        } else {
            libc::snprintf(
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                format.as_ptr().cast(),
                value,
            )
        }
    };
    if len < 0 || len as usize >= bytes.len() {
        return Err(RenderError::Invalid("floating-point formatting failed"));
    }
    let bytes = &bytes[..len as usize];
    let extra = if value.is_finite()
        && (!matches!(spec.kind, b'g' | b'G') || spec.flags & ALTERNATE != 0)
    {
        spec.precision.unwrap_or_default().saturating_sub(EXACT)
    } else {
        0
    };
    let exponent = match spec.kind {
        b'a' | b'A' => b"pP",
        _ => b"eE",
    };
    let tail = bytes
        .iter()
        .rposition(|byte| exponent.contains(byte))
        .unwrap_or(bytes.len());
    let pad = spec.width.saturating_sub(bytes.len() + extra);
    let left = spec.flags & LEFT != 0;
    let zeros = !left && spec.flags & ZERO != 0 && value.is_finite();
    if !left && !zeros {
        writer.pad(b' ', pad)?;
    }
    let mut prefix = usize::from(bytes.first().is_some_and(|byte| b"+- ".contains(byte)));
    if bytes[prefix..].starts_with(b"0x") || bytes[prefix..].starts_with(b"0X") {
        prefix += 2;
    }
    writer.write(&bytes[..prefix])?;
    if zeros {
        writer.pad(b'0', pad)?;
    }
    writer.write(&bytes[prefix..tail])?;
    writer.pad(b'0', extra)?;
    writer.write(&bytes[tail..])?;
    if left {
        writer.pad(b' ', pad)?;
    }
    Ok(())
}
