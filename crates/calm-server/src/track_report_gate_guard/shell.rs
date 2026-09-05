//! Read the literal arguments of the first simple shell command without
//! executing it. Expansions, substitutions, heredocs and compound syntax are
//! deliberately unknown; they must never become grounds for rejecting a gate.

fn finish_word(
    words: &mut Vec<String>,
    word: &mut String,
    started: &mut bool,
    redirect: &mut bool,
) {
    if !*started {
        return;
    }
    if *redirect {
        word.clear();
        *redirect = false;
    } else {
        words.push(std::mem::take(word));
    }
    *started = false;
}

pub(super) fn first_literal_command(command: &str) -> Option<Vec<String>> {
    let mut chars = command.chars().peekable();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut plain = true;
    let mut quote = None;
    let mut redirect = false;
    while let Some(ch) = chars.next() {
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        match ch {
            '\\' => {
                let escaped = chars.next()?;
                // Within double quotes, sh retains a backslash unless it
                // escapes one of these four characters (or a newline).
                if quote == Some('"') && !matches!(escaped, '$' | '`' | '"' | '\\' | '\n') {
                    word.push('\\');
                }
                if escaped != '\n' {
                    word.push(escaped);
                    started = true;
                }
                plain = false;
            }
            '"' => {
                quote = if quote == Some('"') { None } else { Some('"') };
                started = true;
                plain = false;
            }
            '\'' if quote.is_none() => {
                quote = Some('\'');
                started = true;
                plain = false;
            }
            '$' | '`' => return None,
            _ if quote.is_some() => word.push(ch),
            ' ' | '\t' => {
                finish_word(&mut words, &mut word, &mut started, &mut redirect);
                plain = true;
            }
            '#' if !started => break,
            '|' | '&' | ';' | '\n' => break,
            '>' | '<' => {
                if redirect {
                    return None;
                }
                // An adjacent, unquoted digit word is an fd, not an argv
                // entry: `neige 2>/dev/null state` still invokes `state`.
                if started && plain && word.bytes().all(|b| b.is_ascii_digit()) {
                    word.clear();
                    started = false;
                }
                finish_word(&mut words, &mut word, &mut started, &mut redirect);
                if ch == '<' && chars.peek() == Some(&'<') {
                    return None;
                }
                if matches!(
                    (ch, chars.peek()),
                    ('>', Some('>' | '|' | '&')) | ('<', Some('>' | '&'))
                ) {
                    chars.next();
                }
                redirect = true;
                plain = true;
            }
            '(' | ')' | '{' | '}' | '*' | '?' | '[' | '~' => return None,
            _ => {
                word.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    finish_word(&mut words, &mut word, &mut started, &mut redirect);
    if redirect { None } else { Some(words) }
}
