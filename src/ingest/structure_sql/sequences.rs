use sqlparser::tokenizer::{Token, TokenWithSpan};

use super::{starts, word};

// sqlparser requires a fixed option order and omits pg_dump's SEQUENCE NAME.
pub(super) fn normalize(tokens: &[TokenWithSpan]) -> Vec<TokenWithSpan> {
    let mut out = tokens.to_vec();
    let mut ranges = Vec::new();
    for (i, pair) in tokens.windows(2).enumerate() {
        if word(&pair[0].token, "IDENTITY") && pair[1].token == Token::LParen {
            if let Some(end) = tokens[i + 2..]
                .iter()
                .position(|t| t.token == Token::RParen)
            {
                ranges.push((i + 2, i + 2 + end));
            }
        }
    }
    if starts(tokens, &["CREATE", "SEQUENCE"]) {
        if let Some(start) = tokens.iter().position(|t| rank(t).is_some_and(|r| r != 7)) {
            let end = tokens
                .iter()
                .position(|t| word(&t.token, "OWNED"))
                .unwrap_or(tokens.len());
            ranges.push((start, end));
        }
    }
    for (start, end) in ranges.into_iter().rev() {
        if let Some(options) = options(&tokens[start..end]) {
            out.splice(start..end, options);
        }
    }
    out
}

fn rank(t: &TokenWithSpan) -> Option<usize> {
    [
        "INCREMENT",
        "MINVALUE",
        "MAXVALUE",
        "START",
        "CACHE",
        "CYCLE",
        "NO",
        "SEQUENCE",
    ]
    .iter()
    .position(|k| word(&t.token, k))
}

fn options(tokens: &[TokenWithSpan]) -> Option<Vec<TokenWithSpan>> {
    let mut groups = std::collections::BTreeMap::new();
    let mut i = 0;
    while i < tokens.len() {
        let start = i;
        let mut order = rank(&tokens[i])?;
        i += 1;
        if order == 7 {
            if !tokens.get(i).is_some_and(|t| word(&t.token, "NAME")) {
                return None;
            }
            i += 1;
            if !tokens
                .get(i)
                .is_some_and(|t| matches!(t.token, Token::Word(_)))
            {
                return None;
            }
            i += 1;
            while tokens.get(i).is_some_and(|t| t.token == Token::Period) {
                i += 1;
                if !tokens
                    .get(i)
                    .is_some_and(|t| matches!(t.token, Token::Word(_)))
                {
                    return None;
                }
                i += 1;
            }
            continue;
        }
        if order == 6 {
            order = rank(tokens.get(i)?)?;
            if !matches!(order, 1 | 2 | 5) {
                return None;
            }
            i += 1;
        } else if order != 5 {
            if tokens
                .get(i)
                .is_some_and(|t| word(&t.token, "BY") || word(&t.token, "WITH"))
            {
                i += 1;
            }
            if tokens
                .get(i)
                .is_some_and(|t| matches!(t.token, Token::Minus | Token::Plus))
            {
                i += 1;
            }
            if !tokens
                .get(i)
                .is_some_and(|t| matches!(t.token, Token::Number(..)))
            {
                return None;
            }
            i += 1;
        }
        if groups.insert(order, tokens[start..i].to_vec()).is_some() {
            return None;
        }
    }
    Some(groups.into_values().flatten().collect())
}
