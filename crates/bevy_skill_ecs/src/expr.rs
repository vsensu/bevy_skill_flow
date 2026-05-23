use crate::{
    SkillContext, SkillError, SkillExpr, SkillParams as SkillArgs, SkillSpecialValue, SkillValue,
};
use indexmap::IndexMap;

pub fn eval_skill_expr(expr: &SkillExpr, ctx: &SkillContext) -> Result<SkillValue, SkillError> {
    let mut parser = Parser::new(&expr.0).map_err(|message| SkillError::Expr {
        expr: expr.0.clone(),
        message,
    })?;
    let value = parser.parse_expr(ctx).map_err(|message| SkillError::Expr {
        expr: expr.0.clone(),
        message,
    })?;
    if parser.peek().is_some() {
        return Err(SkillError::Expr {
            expr: expr.0.clone(),
            message: "unexpected trailing token".to_owned(),
        });
    }
    Ok(value)
}

pub fn resolve_value(value: &SkillValue, ctx: &SkillContext) -> Result<SkillValue, SkillError> {
    match value {
        SkillValue::Special(SkillSpecialValue::Expr(expr)) => {
            eval_skill_expr(&SkillExpr(expr.clone()), ctx)
        }
        // RON 0.8 erases the enum constructor for values parsed through
        // untagged data, so Expr("stat.x") can arrive as a one-item sequence.
        SkillValue::List(values)
            if values.len() == 1
                && matches!(values.first(), Some(SkillValue::String(expr)) if looks_like_expr(expr)) =>
        {
            if let Some(SkillValue::String(expr)) = values.first() {
                eval_skill_expr(&SkillExpr(expr.clone()), ctx)
            } else {
                unreachable!()
            }
        }
        SkillValue::Map(map) => map
            .iter()
            .map(|(key, value)| Ok((key.clone(), resolve_value(value, ctx)?)))
            .collect::<Result<IndexMap<_, _>, _>>()
            .map(SkillValue::Map),
        SkillValue::List(list) => list
            .iter()
            .map(|value| resolve_value(value, ctx))
            .collect::<Result<Vec<_>, _>>()
            .map(SkillValue::List),
        other => Ok(other.clone()),
    }
}

fn looks_like_expr(expr: &str) -> bool {
    expr.starts_with("stat.")
        || expr.starts_with("event.")
        || expr.starts_with("var.")
        || expr.starts_with("vars.")
        || expr.contains(" ?? ")
        || expr.contains(" * ")
        || expr.contains(" + ")
        || expr.contains(" - ")
        || expr.contains(" / ")
        || expr.contains(" == ")
        || expr.contains(" != ")
        || expr.contains(" >= ")
        || expr.contains(" <= ")
        || expr.contains(" && ")
        || expr.contains(" || ")
}

pub fn resolve_args(args: &SkillArgs, ctx: &SkillContext) -> Result<SkillArgs, SkillError> {
    args.iter()
        .map(|(key, value)| Ok((key.clone(), resolve_value(value, ctx)?)))
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(input: &str) -> Result<Self, String> {
        Ok(Self {
            tokens: tokenize(input)?,
            index: 0,
        })
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.peek().cloned()?;
        self.index += 1;
        Some(token)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Token::Op(found)) if *found == op) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn parse_expr(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        self.parse_nullish(ctx)
    }

    fn parse_nullish(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        let mut left = self.parse_or(ctx)?;
        while self.eat_op("??") {
            let right = self.parse_or(ctx)?;
            if matches!(left, SkillValue::Null) {
                left = right;
            }
        }
        Ok(left)
    }

    fn parse_or(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        let mut left = self.parse_and(ctx)?;
        while self.eat_op("||") {
            let right = self.parse_and(ctx)?;
            left = SkillValue::Bool(truthy(&left) || truthy(&right));
        }
        Ok(left)
    }

    fn parse_and(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        let mut left = self.parse_equality(ctx)?;
        while self.eat_op("&&") {
            let right = self.parse_equality(ctx)?;
            left = SkillValue::Bool(truthy(&left) && truthy(&right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        let mut left = self.parse_comparison(ctx)?;
        loop {
            if self.eat_op("==") {
                let right = self.parse_comparison(ctx)?;
                left = SkillValue::Bool(left == right);
            } else if self.eat_op("!=") {
                let right = self.parse_comparison(ctx)?;
                left = SkillValue::Bool(left != right);
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_comparison(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        let mut left = self.parse_term(ctx)?;
        loop {
            let op = match self.peek() {
                Some(Token::Op("<")) => "<",
                Some(Token::Op("<=")) => "<=",
                Some(Token::Op(">")) => ">",
                Some(Token::Op(">=")) => ">=",
                _ => break,
            };
            self.bump();
            let right = self.parse_term(ctx)?;
            let (a, b) = (number(&left)?, number(&right)?);
            left = SkillValue::Bool(match op {
                "<" => a < b,
                "<=" => a <= b,
                ">" => a > b,
                ">=" => a >= b,
                _ => unreachable!(),
            });
        }
        Ok(left)
    }

    fn parse_term(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        let mut left = self.parse_factor(ctx)?;
        loop {
            if self.eat_op("+") {
                let right = self.parse_factor(ctx)?;
                left = SkillValue::Number(number(&left)? + number(&right)?);
            } else if self.eat_op("-") {
                let right = self.parse_factor(ctx)?;
                left = SkillValue::Number(number(&left)? - number(&right)?);
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_factor(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        let mut left = self.parse_unary(ctx)?;
        loop {
            if self.eat_op("*") {
                let right = self.parse_unary(ctx)?;
                left = SkillValue::Number(number(&left)? * number(&right)?);
            } else if self.eat_op("/") {
                let right = self.parse_unary(ctx)?;
                left = SkillValue::Number(number(&left)? / number(&right)?);
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        if self.eat_op("-") {
            return Ok(SkillValue::Number(-number(&self.parse_unary(ctx)?)?));
        }
        if self.eat_op("!") {
            return Ok(SkillValue::Bool(!truthy(&self.parse_unary(ctx)?)));
        }
        self.parse_primary(ctx)
    }

    fn parse_primary(&mut self, ctx: &SkillContext) -> Result<SkillValue, String> {
        match self.bump() {
            Some(Token::Number(value)) => Ok(SkillValue::Number(value)),
            Some(Token::Str(value)) => Ok(SkillValue::String(value)),
            Some(Token::Ident(ident)) if ident == "true" => Ok(SkillValue::Bool(true)),
            Some(Token::Ident(ident)) if ident == "false" => Ok(SkillValue::Bool(false)),
            Some(Token::Ident(ident)) if ident == "null" => Ok(SkillValue::Null),
            Some(Token::Ident(ident)) => Ok(resolve_path(&ident, ctx).unwrap_or(SkillValue::Null)),
            Some(Token::LParen) => {
                let value = self.parse_expr(ctx)?;
                match self.bump() {
                    Some(Token::RParen) => Ok(value),
                    _ => Err("expected `)`".to_owned()),
                }
            }
            Some(other) => Err(format!("unexpected token `{other:?}`")),
            None => Err("unexpected end of expression".to_owned()),
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch.is_ascii_digit() || ch == '.' && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit())
        {
            let start = i;
            i += 1;
            while chars
                .get(i)
                .is_some_and(|c| c.is_ascii_digit() || *c == '.')
            {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let value = text
                .parse::<f64>()
                .map_err(|_| format!("invalid number `{text}`"))?;
            tokens.push(Token::Number(value));
            continue;
        }
        if ch == '"' {
            i += 1;
            let start = i;
            while chars.get(i).is_some_and(|c| *c != '"') {
                i += 1;
            }
            if i >= chars.len() {
                return Err("unterminated string literal".to_owned());
            }
            tokens.push(Token::Str(chars[start..i].iter().collect()));
            i += 1;
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' || ch == '$' {
            let start = i;
            i += 1;
            while chars
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.' || *c == '$')
            {
                i += 1;
            }
            tokens.push(Token::Ident(chars[start..i].iter().collect()));
            continue;
        }
        if ch == '(' {
            tokens.push(Token::LParen);
            i += 1;
            continue;
        }
        if ch == ')' {
            tokens.push(Token::RParen);
            i += 1;
            continue;
        }
        let two = chars
            .get(i..i + 2)
            .map(|slice| slice.iter().collect::<String>())
            .unwrap_or_default();
        if let Some(op) = match two.as_str() {
            "??" => Some("??"),
            "&&" => Some("&&"),
            "||" => Some("||"),
            "==" => Some("=="),
            "!=" => Some("!="),
            "<=" => Some("<="),
            ">=" => Some(">="),
            _ => None,
        } {
            tokens.push(Token::Op(op));
            i += 2;
            continue;
        }
        if let Some(op) = match ch {
            '+' => Some("+"),
            '-' => Some("-"),
            '*' => Some("*"),
            '/' => Some("/"),
            '<' => Some("<"),
            '>' => Some(">"),
            '!' => Some("!"),
            _ => None,
        } {
            tokens.push(Token::Op(op));
            i += 1;
            continue;
        }
        return Err(format!("unexpected character `{ch}`"));
    }
    Ok(tokens)
}

fn resolve_path(path: &str, ctx: &SkillContext) -> Option<SkillValue> {
    let (root, rest) = path.split_once('.').unwrap_or((path, ""));
    let value = match root {
        "stat" => get_path(&ctx.stats, rest),
        "var" | "vars" => get_path(&ctx.vars, rest),
        "event" => ctx
            .source_event
            .as_ref()
            .and_then(|event| get_path(&event.payload, rest)),
        "skill" if rest == "id" => Some(SkillValue::String(ctx.skill_id.0.clone())),
        name => ctx
            .vars
            .get(name)
            .cloned()
            .or_else(|| ctx.stats.get(name).cloned()),
    };
    value
}

fn get_path(map: &SkillArgs, path: &str) -> Option<SkillValue> {
    if path.is_empty() {
        return Some(SkillValue::Map(map.clone()));
    }
    let mut parts = path.split('.');
    let first = parts.next()?;
    let mut value = map.get(first)?.clone();
    for part in parts {
        value = match value {
            SkillValue::Map(map) => map.get(part)?.clone(),
            _ => return None,
        };
    }
    Some(value)
}

fn number(value: &SkillValue) -> Result<f64, String> {
    match value {
        SkillValue::Number(value) => Ok(*value),
        other => Err(format!("expected number, got `{other:?}`")),
    }
}

fn truthy(value: &SkillValue) -> bool {
    match value {
        SkillValue::Bool(value) => *value,
        SkillValue::Number(value) => *value != 0.0,
        SkillValue::String(value) => !value.is_empty(),
        SkillValue::Null => false,
        SkillValue::List(value) => !value.is_empty(),
        SkillValue::Map(value) => !value.is_empty(),
        SkillValue::Special(_) => true,
    }
}
