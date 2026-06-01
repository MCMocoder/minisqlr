use std::fmt::Display;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenType {
    Ident(Vec<u8>),
    Const(Vec<u8>),
    Oper(Vec<u8>),
}

impl Display for TokenType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenType::Ident(v) => write!(f, "Ident:{}", String::from_utf8_lossy(&v)),
            TokenType::Const(v) => write!(f, "Const:{}", String::from_utf8_lossy(&v)),
            TokenType::Oper(v) => write!(f, "Oper:{}", String::from_utf8_lossy(&v)),
        }
    }
}

fn lex_ident(e: &[u8], iter: &mut usize) -> TokenType {
    let mut buf = Vec::<u8>::new();
    let mut c = e[*iter];
    while (c.is_ascii_alphanumeric() || c == b'_') && *iter < e.len() {
        buf.push(c);
        *iter = *iter + 1;
        if *iter >= e.len() {
            break;
        }
        c = e[*iter];
    }
    return TokenType::Ident(buf);
}

fn lex_num(e: &[u8], iter: &mut usize) -> TokenType {
    let mut buf = Vec::<u8>::new();
    let mut c = e[*iter];
    let mut seen_dot = false;
    while (c.is_ascii_digit() || (c == b'.' && !seen_dot)) && *iter < e.len() {
        if c == b'.' {
            seen_dot = true;
        }
        buf.push(c);
        *iter = *iter + 1;
        if *iter >= e.len() {
            break;
        }
        c = e[*iter];
    }
    return TokenType::Const(buf);
}

fn lex_string(e: &[u8], iter: &mut usize) -> TokenType {
    let mut buf = Vec::<u8>::new();
    *iter += 1;

    while *iter < e.len() {
        let c = e[*iter];
        *iter += 1;
        if c == b'\'' {
            if *iter < e.len() && e[*iter] == b'\'' {
                buf.push(b'\'');
                *iter += 1;
            } else {
                break;
            }
        } else {
            buf.push(c);
        }
    }

    TokenType::Const(buf)
}

fn lex_oper(e: &[u8], iter: &mut usize) -> TokenType {
    let mut buf = vec![e[*iter]];
    *iter += 1;

    if *iter < e.len() {
        let next = e[*iter];
        let two_char_oper = matches!(
            (buf[0], next),
            (b'<', b'=') | (b'>', b'=') | (b'!', b'=') | (b'<', b'>') | (b'=', b'=')
        );
        if two_char_oper {
            buf.push(next);
            *iter += 1;
        }
    }

    TokenType::Oper(buf)
}

pub fn lex_sqlexpr(e: &[u8]) -> Vec<TokenType> {
    let mut tokens = Vec::<TokenType>::new();
    let mut iter = 0;
    while iter < e.len() {
        let c = e[iter];
        if c.is_ascii_alphabetic() || c == b'_' {
            tokens.push(lex_ident(e, &mut iter));
        } else if c.is_ascii_digit() {
            tokens.push(lex_num(e, &mut iter));
        } else if c == b'\'' {
            tokens.push(lex_string(e, &mut iter));
        } else if c.is_ascii_whitespace() {
            iter = iter + 1;
        } else {
            tokens.push(lex_oper(e, &mut iter));
        }
    }
    return tokens;
}
