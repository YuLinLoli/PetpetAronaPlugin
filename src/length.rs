//! 模板长度/坐标表达式求值（移植 petpet-core 的 Length / DynamicLength 子集）。
//!
//! 支持 `100` `100px` `50vw` `50vh` `100cw` `100ch` `50%`，
//! 以及 `50% + 20px`、`100vw - 100cw`、`to_width * 2` 这类加减乘除与变量引用。

use std::collections::HashMap;

/// 百分号相对哪条轴换算（x/宽度按画布宽，y/高度按画布高）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

/// 求值环境：画布尺寸 + 变量表（如 `to_width`、`cover_height`）
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub canvas_w: f32,
    pub canvas_h: f32,
    /// 当前元素（源图）尺寸：petpet 的 `cw`/`ch` 相对它换算
    pub element_w: f32,
    pub element_h: f32,
    /// `%` 的换算基准：默认跟画布，进入元素（crop / 元素内坐标）后改成元素尺寸
    pub percent_w: f32,
    pub percent_h: f32,
    pub vars: HashMap<String, f32>,
}

impl Ctx {
    pub fn new(canvas_w: f32, canvas_h: f32) -> Ctx {
        Ctx {
            canvas_w,
            canvas_h,
            element_w: 0.0,
            element_h: 0.0,
            percent_w: canvas_w,
            percent_h: canvas_h,
            vars: HashMap::new(),
        }
    }

    pub fn set_canvas(&mut self, w: f32, h: f32) {
        self.canvas_w = w;
        self.canvas_h = h;
        self.percent_w = w;
        self.percent_h = h;
    }

    /// 求值某个元素自己的 coords/crop 前调用：`cw`/`ch` 从此按它的尺寸算
    pub fn set_element(&mut self, w: f32, h: f32) {
        self.element_w = w;
        self.element_h = h;
        self.percent_w = w;
        self.percent_h = h;
    }

    pub fn put(&mut self, name: impl Into<String>, value: f32) {
        self.vars.insert(name.into(), value);
    }

    /// `%` 之外的相对单位都直接挂画布尺寸；`%` 按传入的轴取基准
    fn unit_factor(&self, unit: Unit, axis: Axis) -> f32 {
        match unit {
            Unit::Px => 1.0,
            Unit::Vw => self.canvas_w / 100.0,
            Unit::Vh => self.canvas_h / 100.0,
            // cw/ch 相对元素尺寸；元素尺寸未知时退回画布，避免老模板算成 0
            Unit::Cw => {
                if self.element_w != 0.0 {
                    self.element_w / 100.0
                } else {
                    self.canvas_w / 100.0
                }
            }
            Unit::Ch => {
                if self.element_h != 0.0 {
                    self.element_h / 100.0
                } else {
                    self.canvas_h / 100.0
                }
            }
            Unit::Percent => match axis {
                Axis::X => self.percent_w / 100.0,
                Axis::Y => self.percent_h / 100.0,
            },
        }
    }

    pub fn eval(&self, node: &Node, axis: Axis) -> f32 {
        match node {
            Node::Num(value, unit) => value * self.unit_factor(*unit, axis),
            Node::Var(name) => self.vars.get(name).copied().unwrap_or(0.0),
            Node::Add(a, b) => self.eval(a, axis) + self.eval(b, axis),
            Node::Sub(a, b) => self.eval(a, axis) - self.eval(b, axis),
            Node::Mul(a, b) => self.eval(a, axis) * self.eval(b, axis),
            Node::Div(a, b) => {
                let divisor = self.eval(b, axis);
                if divisor.abs() < f32::EPSILON {
                    0.0
                } else {
                    self.eval(a, axis) / divisor
                }
            }
            Node::Neg(a) => -self.eval(a, axis),
            Node::Min(a, b) => self.eval(a, axis).min(self.eval(b, axis)),
            Node::Max(a, b) => self.eval(a, axis).max(self.eval(b, axis)),
        }
    }
}

/// 表达式节点
#[derive(Clone, Debug)]
pub enum Node {
    Num(f32, Unit),
    Var(String),
    Add(Box<Node>, Box<Node>),
    Sub(Box<Node>, Box<Node>),
    Mul(Box<Node>, Box<Node>),
    Div(Box<Node>, Box<Node>),
    Neg(Box<Node>),
    /// `min(a, b)` / `max(a, b)`：petpet 的动态长度表达式里很常见
    Min(Box<Node>, Box<Node>),
    Max(Box<Node>, Box<Node>),
}

impl Node {
    pub fn num(value: f32) -> Node {
        Node::Num(value, Unit::Px)
    }

    pub fn px(value: f32) -> Node {
        Node::Num(value, Unit::Px)
    }

    pub fn percent(value: f32) -> Node {
        Node::Num(value, Unit::Percent)
    }
    /// 相对元素宽度的百分比（petpet 的 `cw`）
    pub fn cw(value: f32) -> Node {
        Node::Num(value, Unit::Cw)
    }

    /// 相对画布宽度的百分比（petpet 的 `vw`）
    pub fn vw(value: f32) -> Node {
        Node::Num(value, Unit::Vw)
    }

    /// 相对画布高度的百分比（petpet 的 `vh`）
    pub fn vh(value: f32) -> Node {
        Node::Num(value, Unit::Vh)
    }

    /// 相对元素高度的百分比（petpet 的 `ch`）
    pub fn ch(value: f32) -> Node {
        Node::Num(value, Unit::Ch)
    }

    /// 换算成"固定像素值"。表达式里出现变量或相对单位时返回 None。
    pub fn fixed_px(&self) -> Option<f32> {
        match self {
            Node::Num(value, Unit::Px) => Some(*value),
            Node::Neg(node) => node.fixed_px().map(|v| -v),
            Node::Min(a, b) => Some(a.fixed_px()?.min(b.fixed_px()?)),
            Node::Max(a, b) => Some(a.fixed_px()?.max(b.fixed_px()?)),
            Node::Add(a, b) => Some(a.fixed_px()? + b.fixed_px()?),
            Node::Sub(a, b) => Some(a.fixed_px()? - b.fixed_px()?),
            Node::Mul(a, b) => Some(a.fixed_px()? * b.fixed_px()?),
            Node::Div(a, b) => {
                let divisor = b.fixed_px()?;
                if divisor.abs() < f32::EPSILON {
                    None
                } else {
                    Some(a.fixed_px()? / divisor)
                }
            }
            _ => None,
        }
    }

    /// 变量名集合（画布尺寸依赖分析用）
    pub fn collect_vars(&self, out: &mut Vec<String>) {
        match self {
            Node::Var(name) => out.push(name.clone()),
            Node::Num(..) => {}
            Node::Neg(a) => a.collect_vars(out),
            Node::Add(a, b)
            | Node::Sub(a, b)
            | Node::Mul(a, b)
            | Node::Div(a, b)
            | Node::Min(a, b)
            | Node::Max(a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Px,
    Vw,
    Vh,
    Cw,
    Ch,
    Percent,
}

/// 解析一段长度表达式。失败返回中文原因。
pub fn parse(input: &str) -> Result<Node, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("长度表达式为空".to_string());
    }
    let mut parser = Parser {
        chars: trimmed.chars().collect(),
        pos: 0,
    };
    let node = parser.expr()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(format!("长度表达式无法完整解析: {input}"));
    }
    Ok(node)
}

/// 从 JSON 值解析长度：数字直接是 px，字符串走表达式
pub fn from_json(value: &serde_json::Value) -> Result<Node, String> {
    match value {
        serde_json::Value::Number(number) => {
            Ok(Node::px(number.as_f64().unwrap_or(0.0) as f32))
        }
        serde_json::Value::String(text) => parse(text),
        other => Err(format!("长度字段类型不支持: {other}")),
    }
}

/// 从 JSON 值解析长度，可回退默认值
pub fn from_json_or(value: Option<&serde_json::Value>, fallback: Node) -> Node {
    value.and_then(|v| from_json(v).ok()).unwrap_or(fallback)
}

/// [`Ctx`] 上的求值快捷方式
pub fn eval(value: &serde_json::Value, ctx: &Ctx, axis: Axis) -> Option<f32> {
    from_json(value).ok().map(|node| ctx.eval(&node, axis))
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, expected: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<Node, String> {
        let mut node = self.term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.pos += 1;
                    let rhs = self.term()?;
                    node = Node::Add(Box::new(node), Box::new(rhs));
                }
                Some('-') => {
                    self.pos += 1;
                    let rhs = self.term()?;
                    node = Node::Sub(Box::new(node), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(node)
    }

    fn term(&mut self) -> Result<Node, String> {
        let mut node = self.unary()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    node = Node::Mul(Box::new(node), Box::new(rhs));
                }
                Some('/') => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    node = Node::Div(Box::new(node), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(node)
    }

    fn unary(&mut self) -> Result<Node, String> {
        self.skip_ws();
        if self.eat('-') {
            Ok(Node::Neg(Box::new(self.unary()?)))
        } else if self.eat('+') {
            self.unary()
        } else {
            self.primary()
        }
    }

    fn primary(&mut self) -> Result<Node, String> {
        self.skip_ws();
        if self.eat('(') {
            let node = self.expr()?;
            if !self.eat(')') {
                return Err("长度表达式缺少右括号".to_string());
            }
            return Ok(node);
        }
        match self.peek() {
            Some(c) if c.is_ascii_digit() || c == '.' => self.number(),
            Some(c) if c.is_alphabetic() || c == '_' => {
                let name = self.ident();
                if name.is_empty() {
                    return Err("长度表达式出现空标识符".to_string());
                }
                // `min(a, b)` / `max(a, b)`：petpet 的动态长度表达式常用
                if self.eat('(') {
                    let first = self.expr()?;
                    if !self.eat(',') {
                        return Err(format!("{name}() 需要两个参数"));
                    }
                    let second = self.expr()?;
                    if !self.eat(')') {
                        return Err("长度表达式缺少右括号".to_string());
                    }
                    return match name.as_str() {
                        "min" => Ok(Node::Min(Box::new(first), Box::new(second))),
                        "max" => Ok(Node::Max(Box::new(first), Box::new(second))),
                        other => Err(format!("不支持的长度函数: {other}")),
                    };
                }
                Ok(Node::Var(name))
            }
            other => Err(format!("长度表达式出现意外字符: {other:?}")),
        }
    }

    fn number(&mut self) -> Result<Node, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        let value: f32 = text
            .parse()
            .map_err(|_| format!("非法数字: {text}"))?;
        self.skip_ws();
        if self.peek() == Some('%') {
            self.pos += 1;
            return Ok(Node::Num(value, Unit::Percent));
        }
        let unit_text = self.ident();
        let unit = match unit_text.as_str() {
            "" | "px" => Unit::Px,
            "vw" => Unit::Vw,
            "vh" => Unit::Vh,
            "cw" => Unit::Cw,
            "ch" => Unit::Ch,
            other => return Err(format!("未知长度单位: {other}")),
        };
        Ok(Node::Num(value, unit))
    }

    fn ident(&mut self) -> String {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }
}

/// 相对偏移（`position` / `origin`）：`"left top"`、`"center"`、`"20px 80%"`。
#[derive(Clone, Debug)]
pub struct Offset {
    pub x: Node,
    pub y: Node,
}

impl Default for Offset {
    fn default() -> Self {
        Offset::center()
    }
}

impl Offset {
    pub fn center() -> Offset {
        Offset {
            x: Node::percent(50.0),
            y: Node::percent(50.0),
        }
    }

    pub fn zero() -> Offset {
        Offset {
            x: Node::px(0.0),
            y: Node::px(0.0),
        }
    }

    /// 相对 `base`（通常是容器尺寸减内容尺寸）求值
    pub fn eval(&self, base_w: f32, base_h: f32) -> (f32, f32) {
        let mut ctx = Ctx::new(base_w, base_h);
        ctx.canvas_w = base_w;
        ctx.canvas_h = base_h;
        (ctx.eval(&self.x, Axis::X), ctx.eval(&self.y, Axis::Y))
    }
}

/// 解析偏移字符串；无法解析时退回居中
pub fn parse_offset(input: &str) -> Offset {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Offset::center();
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let (x_token, y_token) = match tokens.len() {
        0 => return Offset::center(),
        1 => {
            let token = tokens[0];
            let lower = token.to_ascii_lowercase();
            match lower.as_str() {
                "center" => return Offset::center(),
                "left" | "right" => (token, "center"),
                "top" | "bottom" => ("center", token),
                _ => (token, token),
            }
        }
        _ => (tokens[0], tokens[1]),
    };
    Offset {
        x: axis_node(x_token, Axis::X).unwrap_or_else(|| Node::percent(50.0)),
        y: axis_node(y_token, Axis::Y).unwrap_or_else(|| Node::percent(50.0)),
    }
}

fn axis_node(token: &str, axis: Axis) -> Option<Node> {
    let lower = token.trim().to_ascii_lowercase();
    match lower.as_str() {
        "left" | "top" => Some(Node::percent(0.0)),
        "center" => Some(Node::percent(50.0)),
        "right" | "bottom" => Some(Node::percent(100.0)),
        _ => {
            let node = parse(token).ok()?;
            // 关键词与百分比都按"剩余空间"换算，px 保持原样
            match axis {
                Axis::X | Axis::Y => Some(node),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx::new(800.0, 400.0)
    }

    #[test]
    fn parses_basic_units() {
        let c = ctx();
        assert_eq!(c.eval(&parse("100").unwrap(), Axis::X), 100.0);
        assert_eq!(c.eval(&parse("50vw").unwrap(), Axis::X), 400.0);
        assert_eq!(c.eval(&parse("50vh").unwrap(), Axis::Y), 200.0);
        assert_eq!(c.eval(&parse("100cw").unwrap(), Axis::X), 800.0);
        assert_eq!(c.eval(&parse("50%").unwrap(), Axis::X), 400.0);
    }

    #[test]
    fn parses_expressions() {
        let c = ctx();
        assert_eq!(c.eval(&parse("50% + 20px").unwrap(), Axis::X), 420.0);
        assert_eq!(c.eval(&parse("100vw - 100cw").unwrap(), Axis::X), 0.0);
        let mut c = c;
        c.put("to_width", 300.0);
        assert_eq!(c.eval(&parse("to_width * 2").unwrap(), Axis::X), 600.0);
    }

    #[test]
    fn offset_keywords() {
        let offset = parse_offset("left top");
        assert_eq!(offset.eval(100.0, 100.0), (0.0, 0.0));
        let offset = parse_offset("center");
        assert_eq!(offset.eval(200.0, 100.0), (100.0, 50.0));
        let offset = parse_offset("right bottom");
        assert_eq!(offset.eval(200.0, 100.0), (200.0, 100.0));
    }
}
