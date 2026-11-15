use crate::shell::Shell;

#[derive(Clone, Copy, Debug)]
enum Expr<'a> {
    Number(i64),
    Name(&'a [u8]),
    Unary(&'a [u8], usize),
    Binary(&'a [u8], usize, usize),
    Choose(usize, usize, usize),
}

/// A small expression needs no Box allocations. Longer expressions grow one
/// vector, with node indices remaining valid across allocation boundaries.
struct Nodes<'a> {
    local: [Expr<'a>; 8],
    used: usize,
    overflow: Vec<Expr<'a>>,
}

impl<'a> Nodes<'a> {
    fn new() -> Self {
        Self {
            local: [Expr::Number(0); 8],
            used: 0,
            overflow: Vec::new(),
        }
    }

    fn push(&mut self, node: Expr<'a>) -> usize {
        let index = self.used;
        self.used += 1;
        if let Some(slot) = self.local.get_mut(index) {
            *slot = node;
        } else {
            self.overflow.push(node);
        }
        index
    }

    fn get(&self, index: usize) -> Expr<'a> {
        if let Some(node) = self.local.get(index) {
            *node
        } else {
            self.overflow[index - self.local.len()]
        }
    }
}

pub fn evaluate(shell: &mut Shell, source: &[u8]) -> Result<i64, String> {
    evaluate_depth(shell, source, 0)
}

fn evaluate_depth(shell: &mut Shell, source: &[u8], depth: usize) -> Result<i64, String> {
    if depth >= 64 {
        return Err("recursive arithmetic expression".into());
    }
    if let Some(value) = plain_integer(source) {
        return Ok(value);
    }
    let mut reader = Reader {
        input: source,
        nodes: Nodes::new(),
    };
    let tree = reader.expression(1)?;
    if !reader.rest().is_empty() {
        return Err("invalid arithmetic expression".into());
    }
    value(shell, &reader.nodes, tree, depth)
}

/// The normal iteration variable is already numeric. Read its bytes in place
/// and leave general shell arithmetic to the parser.
fn plain_integer(source: &[u8]) -> Option<i64> {
    let source = source.trim_ascii();
    if source.is_empty() {
        return Some(0);
    }
    let (negative, digits) = match source {
        [b'-', digits @ ..] => (true, digits),
        [b'+', digits @ ..] => (false, digits),
        digits => (false, digits),
    };
    if digits.is_empty() {
        return None;
    }
    let radix = if digits[0] == b'0' { 8 } else { 10 };
    let value = digits.iter().try_fold(0u64, |n, byte| {
        let digit = u64::from(byte.wrapping_sub(b'0'));
        if digit >= radix {
            return None;
        }
        n.checked_mul(radix)?.checked_add(digit)
    })? as i64;
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

struct Reader<'a> {
    input: &'a [u8],
    nodes: Nodes<'a>,
}

impl<'a> Reader<'a> {
    fn rest(&mut self) -> &'a [u8] {
        self.input = self.input.trim_ascii_start();
        self.input
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let (head, tail) = self.input.split_at(n);
        self.input = tail;
        head
    }

    fn expression(&mut self, min: u8) -> Result<usize, String> {
        let mut left = self.atom()?;
        loop {
            let s = self.rest();
            if min <= 3 && s.starts_with(b"?") {
                self.take(1);
                let yes = self.expression(1)?;
                if !self.rest().starts_with(b":") {
                    return Err("expected ':' in arithmetic expression".into());
                }
                self.take(1);
                let no = self.expression(3)?;
                left = self.nodes.push(Expr::Choose(left, yes, no));
                continue;
            }
            let Some((len, rank, right_assoc)) = operator(s) else {
                break;
            };
            if rank < min {
                break;
            }
            let op = self.take(len);
            let rhs = self.expression(rank + u8::from(!right_assoc))?;
            left = self.nodes.push(Expr::Binary(op, left, rhs));
        }
        Ok(left)
    }

    fn atom(&mut self) -> Result<usize, String> {
        let s = self.rest();
        match s.first().copied() {
            Some(b'(') => {
                self.take(1);
                let expr = self.expression(1)?;
                if !self.rest().starts_with(b")") {
                    return Err("expected ')' in arithmetic expression".into());
                }
                self.take(1);
                Ok(expr)
            }
            Some(b'+' | b'-' | b'!' | b'~') => {
                let op = self.take(1);
                let inner = self.atom()?;
                Ok(self.nodes.push(Expr::Unary(op, inner)))
            }
            Some(b'0'..=b'9') => {
                let n = s.iter().take_while(|c| c.is_ascii_alphanumeric()).count();
                let token = std::str::from_utf8(self.take(n)).unwrap();
                let (digits, radix) = if token.starts_with("0x") || token.starts_with("0X") {
                    (&token[2..], 16)
                } else if token.starts_with('0') {
                    (token, 8)
                } else {
                    (token, 10)
                };
                u64::from_str_radix(digits, radix)
                    .map(|number| self.nodes.push(Expr::Number(number as i64)))
                    .map_err(|_| format!("invalid integer '{token}'"))
            }
            Some(b'a'..=b'z' | b'A'..=b'Z' | b'_') => {
                let n = s
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                    .count();
                let name = self.take(n);
                Ok(self.nodes.push(Expr::Name(name)))
            }
            _ => Err("expected arithmetic operand".into()),
        }
    }
}

fn operator(s: &[u8]) -> Option<(usize, u8, bool)> {
    let (len, rank) = match s {
        [b'<', b'<', b'=', ..] | [b'>', b'>', b'=', ..] => (3, 2),
        [
            b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'^' | b'|',
            b'=',
            ..,
        ] => (2, 2),
        [b'=', b'=', ..] | [b'!', b'=', ..] => (2, 9),
        [b'=', ..] => (1, 2),
        [b'|', b'|', ..] => (2, 4),
        [b'&', b'&', ..] => (2, 5),
        [b'|', ..] => (1, 6),
        [b'^', ..] => (1, 7),
        [b'&', ..] => (1, 8),
        [b'<', b'=', ..] | [b'>', b'=', ..] => (2, 10),
        [b'<', b'<', ..] | [b'>', b'>', ..] => (2, 11),
        [b'<' | b'>', ..] => (1, 10),
        [b'+' | b'-', ..] => (1, 12),
        [b'*' | b'/' | b'%', ..] => (1, 13),
        [b',', ..] => (1, 1),
        _ => return None,
    };
    Some((len, rank, rank == 2))
}

fn value(shell: &mut Shell, tree: &Nodes<'_>, index: usize, depth: usize) -> Result<i64, String> {
    match tree.get(index) {
        Expr::Number(n) => Ok(n),
        Expr::Name(name) => {
            let source = shell.get(name).unwrap_or_default();
            if depth + 1 < 64 {
                if let Some(n) = plain_integer(source) {
                    return Ok(n);
                }
            }
            let source = source.to_vec();
            evaluate_depth(shell, &source, depth + 1)
        }
        Expr::Choose(test, yes, no) => {
            let branch = if value(shell, tree, test, depth)? != 0 {
                yes
            } else {
                no
            };
            value(shell, tree, branch, depth)
        }
        Expr::Unary(op, operand) => {
            let n = value(shell, tree, operand, depth)?;
            Ok(match op {
                b"-" => n.wrapping_neg(),
                b"!" => i64::from(n == 0),
                b"~" => !n,
                _ => n,
            })
        }
        Expr::Binary(op, left, right) => {
            if operator(op).is_some_and(|(_, rank, _)| rank == 2) {
                let Expr::Name(name) = tree.get(left) else {
                    return Err("arithmetic assignment requires a variable".into());
                };
                let rhs = value(shell, tree, right, depth)?;
                let result = if op == b"=" {
                    rhs
                } else {
                    binary(&op[..op.len() - 1], value(shell, tree, left, depth)?, rhs)?
                };
                shell.assign(name, result.to_string().into_bytes(), false)?;
                return Ok(result);
            }
            let lhs = value(shell, tree, left, depth)?;
            match (op, lhs) {
                (b"&&", 0) => return Ok(0),
                (b"||", n) if n != 0 => return Ok(1),
                _ => {}
            }
            binary(op, lhs, value(shell, tree, right, depth)?)
        }
    }
}

fn binary(op: &[u8], a: i64, b: i64) -> Result<i64, String> {
    Ok(match op {
        b"+" => a.wrapping_add(b),
        b"-" => a.wrapping_sub(b),
        b"*" => a.wrapping_mul(b),
        b"/" | b"%" if b == 0 => return Err("division by zero".into()),
        b"/" => a.wrapping_div(b),
        b"%" => a.wrapping_rem(b),
        b"<<" => a.wrapping_shl(b as u32),
        b">>" => a.wrapping_shr(b as u32),
        b"<" => i64::from(a < b),
        b">" => i64::from(a > b),
        b"<=" => i64::from(a <= b),
        b">=" => i64::from(a >= b),
        b"==" => i64::from(a == b),
        b"!=" => i64::from(a != b),
        b"&" => a & b,
        b"^" => a ^ b,
        b"|" => a | b,
        b"&&" | b"||" => i64::from(b != 0),
        b"," => b,
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Options;

    #[test]
    fn long_expression_and_lazy_operators_keep_assignment_order() {
        let mut shell = Shell::new(
            b"nsh".to_vec(),
            vec![],
            Options::default(),
            false,
            false,
            false,
        );
        shell
            .assign(b"nsh_arith_test_i", b"3".to_vec(), false)
            .unwrap();
        let expression = b"1+2+3+4+5+(nsh_arith_test_i+=7)+(0 && 1/0)+(1 ? 2 : 1/0)";
        assert_eq!(evaluate(&mut shell, expression).unwrap(), 27);
        assert_eq!(shell.get(b"nsh_arith_test_i"), Some(b"10".as_slice()));
    }

    #[test]
    fn decimal_and_octal_shortcuts_preserve_u64_wrapping() {
        assert_eq!(plain_integer(b" +010\t"), Some(8));
        assert_eq!(plain_integer(b"-010"), Some(-8));
        assert_eq!(plain_integer(b"18446744073709551615"), Some(-1));
        assert_eq!(plain_integer(b"18446744073709551616"), None);
        assert_eq!(plain_integer(b"08"), None);
        assert_eq!(plain_integer(b"0xA"), None);
    }
}
