//! Ограниченные вещественные формулы и физический ток источника Jz, А/м².

use std::f64::consts::{PI, TAU};

use evalexpr::{Node, Operator, Value, build_operator_tree};

use crate::model::{Profile, Signal, Source};
use crate::{Error, Result};

const MAX_TEXT: usize = 2048;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 512;
const MAX_OPERATORS: usize = 128;
const MAX_SAMPLES: usize = 4096;

#[derive(Clone, Copy, Debug)]
struct Variables {
    t: f64,
    s: f64,
    f: f64,
    phi: f64,
}

#[derive(Clone, Copy, Debug)]
enum Variable {
    Time,
    Position,
    Frequency,
    Phase,
}

#[derive(Clone, Copy, Debug)]
enum Unary {
    Neg,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Exp,
    Sqrt,
    Abs,
    Ln,
    Log10,
    Log2,
    Floor,
    Ceil,
    Round,
    Sinh,
    Cosh,
    Tanh,
}

impl Unary {
    fn evaluate(self, value: f64) -> Result<f64> {
        expression_finite(match self {
            Self::Neg => -value,
            Self::Sin => value.sin(),
            Self::Cos => value.cos(),
            Self::Tan => value.tan(),
            Self::Asin => value.asin(),
            Self::Acos => value.acos(),
            Self::Atan => value.atan(),
            Self::Exp => value.exp(),
            Self::Sqrt => value.sqrt(),
            Self::Abs => value.abs(),
            Self::Ln => value.ln(),
            Self::Log10 => value.log10(),
            Self::Log2 => value.log2(),
            Self::Floor => value.floor(),
            Self::Ceil => value.ceil(),
            Self::Round => value.round(),
            Self::Sinh => value.sinh(),
            Self::Cosh => value.cosh(),
            Self::Tanh => value.tanh(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Min,
    Max,
    Atan2,
    Hypot,
}

impl Binary {
    fn evaluate(self, left: f64, right: f64) -> Result<f64> {
        expression_finite(match self {
            Self::Add => left + right,
            Self::Sub => left - right,
            Self::Mul => left * right,
            Self::Div => left / right,
            Self::Rem => left % right,
            Self::Pow => left.powf(right),
            Self::Min => left.min(right),
            Self::Max => left.max(right),
            Self::Atan2 => left.atan2(right),
            Self::Hypot => left.hypot(right),
        })
    }
}

#[derive(Debug)]
enum Expression {
    Constant(f64),
    Variable(Variable),
    Unary(Unary, Box<Expression>),
    Binary(Binary, Box<Expression>, Box<Expression>),
}

impl Expression {
    fn evaluate(&self, variables: Variables) -> Result<f64> {
        match self {
            Self::Constant(value) => Ok(*value),
            Self::Variable(variable) => Ok(match variable {
                Variable::Time => variables.t,
                Variable::Position => variables.s,
                Variable::Frequency => variables.f,
                Variable::Phase => variables.phi,
            }),
            Self::Unary(operator, child) => operator.evaluate(child.evaluate(variables)?),
            Self::Binary(operator, left, right) => {
                operator.evaluate(left.evaluate(variables)?, right.evaluate(variables)?)
            }
        }
    }

    fn unary(operator: Unary, child: Self) -> Result<Self> {
        if let Self::Constant(value) = child {
            Ok(Self::Constant(operator.evaluate(value)?))
        } else {
            Ok(Self::Unary(operator, Box::new(child)))
        }
    }

    fn binary(operator: Binary, left: Self, right: Self) -> Result<Self> {
        match (left, right) {
            (Self::Constant(left), Self::Constant(right)) => {
                Ok(Self::Constant(operator.evaluate(left, right)?))
            }
            (left, right) => Ok(Self::Binary(operator, Box::new(left), Box::new(right))),
        }
    }
}

fn expression_error(message: &str) -> Error {
    Error::Expression(message.into())
}

fn expression_finite(value: f64) -> Result<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(expression_error(
            "Недопустимая область определения или переполнение выражения",
        ))
    }
}

/// Проверка до вызова стороннего parser ограничивает и плоские цепочки операторов.
fn lexical_check(text: &str) -> Result<()> {
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(expression_error(
            "Длина выражения должна быть от 1 до 2048 байт",
        ));
    }
    let mut depth = 0usize;
    let mut operators = 0usize;
    for byte in text.bytes() {
        match byte {
            b'(' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(expression_error("Вложенность скобок превышает 32"));
                }
            }
            b')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| expression_error("Несогласованные скобки"))?;
            }
            b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b',' => {
                operators += 1;
                if operators > MAX_OPERATORS {
                    return Err(expression_error("Слишком много операторов в выражении"));
                }
            }
            b'0'..=b'9'
            | b'a'..=b'z'
            | b'A'..=b'Z'
            | b'_'
            | b'.'
            | b' '
            | b'\t'
            | b'\r'
            | b'\n' => {}
            _ => {
                return Err(expression_error(
                    "Разрешены только числа, арифметика и математические функции",
                ));
            }
        }
    }
    if depth != 0 {
        return Err(expression_error("Несогласованные скобки"));
    }
    Ok(())
}

#[derive(Debug)]
struct Formula {
    expression: Expression,
    cost: usize,
}

impl Formula {
    fn new(text: &str) -> Result<Self> {
        lexical_check(text)?;
        let tree = build_operator_tree(text)
            .map_err(|_| expression_error("Не удалось разобрать математическое выражение"))?;
        let mut nodes = 0;
        let expression = compile_node(&tree, 0, &mut nodes)?;
        Ok(Self {
            expression,
            cost: nodes * 8,
        })
    }

    fn evaluate(&self, variables: Variables) -> Result<f64> {
        self.expression.evaluate(variables)
    }
}

fn compile_node(node: &Node, depth: usize, count: &mut usize) -> Result<Expression> {
    *count += 1;
    if depth > MAX_DEPTH || *count > MAX_NODES {
        return Err(expression_error(
            "Дерево выражения превышает 512 узлов или глубину 32",
        ));
    }
    let children = node.children();
    match (node.operator(), children) {
        (Operator::RootNode, [child]) => compile_node(child, depth + 1, count),
        (
            Operator::Const {
                value: Value::Int(value),
            },
            [],
        ) => Ok(Expression::Constant(*value as f64)),
        (
            Operator::Const {
                value: Value::Float(value),
            },
            [],
        ) => Ok(Expression::Constant(expression_finite(*value)?)),
        (Operator::VariableIdentifierRead { identifier }, []) => match identifier.as_str() {
            "pi" => Ok(Expression::Constant(PI)),
            "t" => Ok(Expression::Variable(Variable::Time)),
            "s" => Ok(Expression::Variable(Variable::Position)),
            "f" => Ok(Expression::Variable(Variable::Frequency)),
            "phi" => Ok(Expression::Variable(Variable::Phase)),
            _ => Err(expression_error("Допустимые переменные: t, s, f, phi, pi")),
        },
        (Operator::Neg, [child]) => {
            Expression::unary(Unary::Neg, compile_node(child, depth + 1, count)?)
        }
        (operator, [left, right]) => {
            let operator = match operator {
                Operator::Add => Binary::Add,
                Operator::Sub => Binary::Sub,
                Operator::Mul => Binary::Mul,
                Operator::Div => Binary::Div,
                Operator::Mod => Binary::Rem,
                Operator::Exp => Binary::Pow,
                _ => return Err(expression_error("Оператор не разрешён")),
            };
            Expression::binary(
                operator,
                compile_node(left, depth + 1, count)?,
                compile_node(right, depth + 1, count)?,
            )
        }
        (Operator::FunctionIdentifier { identifier }, [argument]) => {
            compile_function(identifier, argument, depth + 1, count)
        }
        _ => Err(expression_error(
            "Разрешены только скалярные математические выражения без присваиваний",
        )),
    }
}

fn compile_function(
    name: &str,
    argument: &Node,
    depth: usize,
    count: &mut usize,
) -> Result<Expression> {
    let unary = match name {
        "sin" => Some(Unary::Sin),
        "cos" => Some(Unary::Cos),
        "tan" => Some(Unary::Tan),
        "asin" => Some(Unary::Asin),
        "acos" => Some(Unary::Acos),
        "atan" => Some(Unary::Atan),
        "exp" => Some(Unary::Exp),
        "sqrt" => Some(Unary::Sqrt),
        "abs" => Some(Unary::Abs),
        "ln" | "log" => Some(Unary::Ln),
        "log10" => Some(Unary::Log10),
        "log2" => Some(Unary::Log2),
        "floor" => Some(Unary::Floor),
        "ceil" => Some(Unary::Ceil),
        "round" => Some(Unary::Round),
        "sinh" => Some(Unary::Sinh),
        "cosh" => Some(Unary::Cosh),
        "tanh" => Some(Unary::Tanh),
        _ => None,
    };
    if let Some(operator) = unary {
        return Expression::unary(operator, compile_node(argument, depth, count)?);
    }
    let operator = match name {
        "min" => Binary::Min,
        "max" => Binary::Max,
        "pow" => Binary::Pow,
        "atan2" => Binary::Atan2,
        "hypot" => Binary::Hypot,
        _ => {
            return Err(expression_error(
                "Функция не входит в разрешённый математический набор",
            ));
        }
    };
    let mut argument = argument;
    let mut depth = depth;
    while matches!(argument.operator(), Operator::RootNode) {
        *count += 1;
        if depth > MAX_DEPTH || *count > MAX_NODES {
            return Err(expression_error("Слишком сложное выражение аргументов"));
        }
        let [child] = argument.children() else {
            return Err(expression_error("Ожидались два аргумента функции"));
        };
        argument = child;
        depth += 1;
    }
    *count += 1;
    if depth > MAX_DEPTH || *count > MAX_NODES {
        return Err(expression_error("Слишком сложное выражение аргументов"));
    }
    match (argument.operator(), argument.children()) {
        (Operator::Tuple, [left, right]) => Expression::binary(
            operator,
            compile_node(left, depth + 1, count)?,
            compile_node(right, depth + 1, count)?,
        ),
        _ => Err(expression_error("Ожидались ровно два аргумента функции")),
    }
}

#[derive(Debug)]
struct Table {
    samples: Vec<[f64; 2]>,
    first: f64,
    last: f64,
}

impl Table {
    fn new(samples: &[[f64; 2]], spatial: bool, periodic: bool) -> Result<Self> {
        if !(2..=MAX_SAMPLES).contains(&samples.len()) {
            return Err(Error::Invalid(
                "Таблица должна содержать от 2 до 4096 отсчётов".into(),
            ));
        }
        if samples.iter().flatten().any(|value| !value.is_finite())
            || samples.windows(2).any(|pair| pair[0][0] >= pair[1][0])
        {
            return Err(Error::Invalid(
                "Отсчёты таблицы должны быть конечны, координаты — строго возрастать".into(),
            ));
        }
        // Размер проверен выше; крайние элементы существуют.
        let first = samples[0][0];
        let last = samples[samples.len() - 1][0];
        if !(last - first).is_finite() || (spatial && (first > -1.0 || last < 1.0)) {
            return Err(Error::Invalid(
                "Некорректный диапазон таблицы; пространственная таблица должна покрывать [-1, 1]"
                    .into(),
            ));
        }
        if periodic && samples[0][1] != samples[samples.len() - 1][1] {
            return Err(Error::Invalid(
                "У периодической таблицы значения на концах периода должны совпадать".into(),
            ));
        }
        Ok(Self {
            samples: samples.to_vec(),
            first,
            last,
        })
    }

    fn evaluate(&self, coordinate: f64, periodic: bool) -> Result<f64> {
        let coordinate = if periodic {
            numerical_finite(coordinate - self.first, "Смещение периодической таблицы")?
                .rem_euclid(self.last - self.first)
                + self.first
        } else {
            coordinate
        };
        if coordinate < self.first || coordinate > self.last {
            return Ok(0.0);
        }
        let upper = self
            .samples
            .partition_point(|sample| sample[0] < coordinate);
        if upper == self.samples.len() {
            return Ok(self.samples[self.samples.len() - 1][1]);
        }
        if upper == 0 || self.samples[upper][0] == coordinate {
            return Ok(self.samples[upper][1]);
        }
        let [left_x, left_y] = self.samples[upper - 1];
        let [right_x, right_y] = self.samples[upper];
        let fraction = (coordinate - left_x) / (right_x - left_x);
        numerical_finite(
            (1.0 - fraction) * left_y + fraction * right_y,
            "Интерполяция таблицы",
        )
    }

    fn cost(&self) -> usize {
        16 + self.samples.len().ilog2() as usize
    }
}

#[derive(Debug)]
enum CompiledProfile {
    Uniform,
    Fundamental,
    Gaussian(f64),
    Formula(Formula),
    Table(Table),
}

impl CompiledProfile {
    fn new(profile: &Profile) -> Result<Self> {
        Ok(match profile {
            Profile::Uniform => Self::Uniform,
            Profile::Fundamental => Self::Fundamental,
            Profile::Gaussian { width } => {
                positive(*width, "Ширина пространственного Gaussian")?;
                Self::Gaussian(*width)
            }
            Profile::Formula { expression } => Self::Formula(Formula::new(expression)?),
            Profile::Table { samples } => Self::Table(Table::new(samples, true, false)?),
        })
    }

    fn evaluate(&self, variables: Variables) -> Result<f64> {
        match self {
            Self::Uniform => Ok(1.0),
            Self::Fundamental => Ok((PI * variables.s / 2.0).cos()),
            Self::Gaussian(width) => gaussian(variables.s / width),
            Self::Formula(formula) => formula.evaluate(variables),
            Self::Table(table) => table.evaluate(variables.s, false),
        }
    }

    fn cost(&self) -> usize {
        match self {
            Self::Uniform => 1,
            Self::Fundamental | Self::Gaussian(_) => 16,
            Self::Formula(formula) => formula.cost,
            Self::Table(table) => table.cost(),
        }
    }
}

#[derive(Debug)]
enum CompiledSignal {
    Continuous { ramp_s: f64 },
    Gaussian { center_s: f64, width_s: f64 },
    Formula(Formula),
    Table { table: Table, periodic: bool },
}

/// Подготовленный источник. Успешное вычисление не выделяет память и не разбирает текст.
///
/// t измеряется в секундах, s находится в [-1, 1], результат — Jz в А/м².
/// Используется f64: фаза и время должны сохранять точность при наносекундных шагах.
/// Для CW/Gaussian carrier = sin(2*pi*f*(t-delay)+phi), где phi = phase_rad +
/// phase_profile. Gaussian имеет exp(-0.5*z²), width — стандартное отклонение.
/// CW плавно включается raised-cosine за ramp_periods/f секунд.
/// Формула задаёт q напрямую; phi доступна ей явно. Таблица не сдвигается фазой.
/// Во всех формулах t — время после задержки; до задержки ток равен нулю.
/// В phase_profile переменная phi означает phase_rad, в aperture и q — суммарную фазу.
/// Uniform в качестве phase_profile даёт 1 радиан; нулевая фаза задаётся Formula("0").
/// log и ln — натуральный логарифм, log10 и log2 имеют соответствующее основание.
/// fmax_hz — декларация пользователя, а не доказанный спектральный предел формулы.
#[derive(Debug)]
pub struct CompiledSource {
    enabled: bool,
    current: f64,
    frequency: f64,
    phase: f64,
    delay: f64,
    signal: CompiledSignal,
    aperture: CompiledProfile,
    phase_profile: CompiledProfile,
    cost: usize,
}

impl CompiledSource {
    pub fn new(source: &Source) -> Result<Self> {
        for value in [source.current_a_m2, source.phase_rad, source.delay_s] {
            if !value.is_finite() {
                return Err(Error::Invalid(
                    "Амплитуда, фаза и задержка должны быть конечны".into(),
                ));
            }
        }
        positive(source.frequency_hz, "Частота источника")?;
        positive(source.fmax_hz, "Верхняя заявленная частота")?;
        if source.fmax_hz < source.frequency_hz || source.delay_s < 0.0 {
            return Err(Error::Invalid(
                "Необходимы fmax >= f и неотрицательная задержка".into(),
            ));
        }
        let signal = match &source.signal {
            Signal::Continuous { ramp_periods } => {
                positive(*ramp_periods, "Число периодов включения CW")?;
                let ramp_s = ramp_periods / source.frequency_hz;
                positive(ramp_s, "Время включения CW")?;
                CompiledSignal::Continuous { ramp_s }
            }
            Signal::Gaussian { center_s, width_s } => {
                positive(*width_s, "Длительность Gaussian")?;
                if !center_s.is_finite() || *center_s < 0.0 {
                    return Err(Error::Invalid(
                        "Центр Gaussian должен быть конечным и неотрицательным".into(),
                    ));
                }
                CompiledSignal::Gaussian {
                    center_s: *center_s,
                    width_s: *width_s,
                }
            }
            Signal::Formula { expression } => CompiledSignal::Formula(Formula::new(expression)?),
            Signal::Table { samples, periodic } => CompiledSignal::Table {
                table: Table::new(samples, false, *periodic)?,
                periodic: *periodic,
            },
        };
        let aperture = CompiledProfile::new(&source.aperture)?;
        let phase_profile = CompiledProfile::new(&source.phase_profile)?;
        let signal_cost = match &signal {
            CompiledSignal::Continuous { .. } | CompiledSignal::Gaussian { .. } => 40,
            CompiledSignal::Formula(formula) => formula.cost,
            CompiledSignal::Table { table, .. } => table.cost() + 8,
        };
        let cost = 16 + signal_cost + aperture.cost() + phase_profile.cost();
        Ok(Self {
            enabled: source.enabled,
            current: source.current_a_m2,
            frequency: source.frequency_hz,
            phase: source.phase_rad,
            delay: source.delay_s,
            signal,
            aperture,
            phase_profile,
            cost,
        })
    }

    pub fn value(&self, t_seconds: f64, s_normalized: f64) -> Result<f64> {
        if !t_seconds.is_finite()
            || !s_normalized.is_finite()
            || !(-1.0..=1.0).contains(&s_normalized)
        {
            return Err(Error::Invalid(
                "Время должно быть конечным, координата апертуры — в [-1, 1]".into(),
            ));
        }
        let t = numerical_finite(t_seconds - self.delay, "Локальное время источника")?;
        if !self.enabled || t < 0.0 {
            return Ok(0.0);
        }
        let mut variables = Variables {
            t,
            s: s_normalized,
            f: self.frequency,
            phi: self.phase,
        };
        variables.phi = numerical_finite(
            self.phase + self.phase_profile.evaluate(variables)?,
            "Фаза источника",
        )?;
        let aperture = self.aperture.evaluate(variables)?;
        let waveform = match &self.signal {
            CompiledSignal::Continuous { ramp_s } => {
                let envelope = if t >= *ramp_s {
                    1.0
                } else {
                    // Эквивалент raised-cosine без вычитания близких к единице чисел.
                    let sine = (0.5 * PI * (t / ramp_s)).sin();
                    sine * sine
                };
                envelope * carrier(variables)?
            }
            CompiledSignal::Gaussian { center_s, width_s } => {
                gaussian((t - center_s) / width_s)? * carrier(variables)?
            }
            CompiledSignal::Formula(formula) => formula.evaluate(variables)?,
            CompiledSignal::Table { table, periodic } => table.evaluate(t, *periodic)?,
        };
        let amplitude = numerical_finite(self.current * aperture, "Амплитуда тока источника")?;
        numerical_finite(amplitude * waveform, "Ток источника Jz")
    }

    /// Условная верхняя оценка операций одного value, не измерение времени CPU.
    pub fn cost(&self) -> usize {
        self.cost
    }
}

fn positive(value: f64, name: &str) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        Err(Error::Invalid(format!(
            "{name}: требуется конечное положительное значение"
        )))
    } else {
        Ok(())
    }
}

fn numerical_finite(value: f64, name: &str) -> Result<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Numerical(format!(
            "{name}: получено неконечное значение"
        )))
    }
}

fn gaussian(normalized: f64) -> Result<f64> {
    let square = numerical_finite(normalized * normalized, "Аргумент Gaussian")?;
    Ok((-0.5 * square).exp())
}

fn carrier(variables: Variables) -> Result<f64> {
    Ok(numerical_finite(
        TAU * (variables.f * variables.t) + variables.phi,
        "Фаза гармонической несущей",
    )?
    .sin())
}
