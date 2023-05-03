use anyhow::{Context, Result};

pub struct Program {
    pub instructions: Vec<Instruction>,
}

impl Program {
    pub fn parse(source_code: &str) -> Result<Self> {
        Ok(Self {
            instructions: source_code
                .lines()
                .flat_map(|line| {
                    line.split_once('#')
                        .map_or(line, |(line, _comment)| line)
                        .split_whitespace()
                })
                .map(Instruction::parse)
                .collect::<Result<_>>()?,
        })
    }
}

pub enum Instruction {
    Push(i32),
    Println,
    PrintChar,
    Add,
    Sub,
    Mul,
    Div,
    SharpS,
    Pop,
    Dup,
    Swap,
}

impl Instruction {
    fn parse(word: &str) -> Result<Self> {
        Ok(match word {
            "p" => Self::Println,
            "c" => Self::PrintChar,
            "+" => Self::Add,
            "-" => Self::Sub,
            "*" => Self::Mul,
            "/" => Self::Div,
            "ß" => Self::SharpS,
            "x" => Self::Pop,
            "d" => Self::Dup,
            "s" => Self::Swap,
            _ => {
                Self::Push(word.parse().ok().with_context(|| {
                    format!("unknown instruction: `{word}`")
                })?)
            }
        })
    }
}
