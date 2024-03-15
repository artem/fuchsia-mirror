// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod test {
    use crate::{EbpfProgramBuilder, NullVerifierLogger};
    use linux_uapi::{
        bpf_insn, BPF_ADD, BPF_ALU, BPF_ALU64, BPF_AND, BPF_ARSH, BPF_CALL, BPF_DIV, BPF_EXIT,
        BPF_IMM, BPF_JA, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JLE, BPF_JLT, BPF_JMP, BPF_JMP32, BPF_JNE,
        BPF_JSET, BPF_JSGE, BPF_JSGT, BPF_JSLE, BPF_JSLT, BPF_LSH, BPF_MOD, BPF_MOV, BPF_MUL,
        BPF_NEG, BPF_OR, BPF_RSH, BPF_SUB, BPF_XOR,
    };
    use pest::{iterators::Pair, Parser};
    use pest_derive::Parser;
    use std::str::FromStr;
    use test_case::test_case;

    #[derive(Parser)]
    #[grammar = "../../src/starnix/lib/ubpf/src/test_grammar.pest"]
    struct TestGrammar {}

    struct TestCase {
        code: Vec<bpf_insn>,
        result: Option<u64>,
        memory: Vec<u8>,
    }

    const BPF_REG: u32 = 0x08;
    const BPF_SWAP: u32 = 0x0d;

    enum Value {
        Plus(u64),
        Minus(u64),
    }

    impl Value {
        fn as_u64(&self) -> u64 {
            match self {
                Self::Plus(v) => *v,
                Self::Minus(v) => -(*v as i64) as u64,
            }
        }

        fn as_i32(&self) -> i32 {
            match self {
                Self::Plus(v) => i32::try_from(*v).unwrap(),
                Self::Minus(v) => -i32::try_from(*v).unwrap(),
            }
        }

        fn as_i16(&self) -> i16 {
            match self {
                Self::Plus(v) => i16::try_from(*v).unwrap(),
                Self::Minus(v) => -i16::try_from(*v).unwrap(),
            }
        }

        #[allow(dead_code)]
        fn as_i32_pair(&self) -> (i32, i32) {
            let v = self.as_u64();
            let (low, high) = (v as i32, (v >> 32) as i32);
            (low, high)
        }
    }

    impl TestCase {
        fn parse_result(pair: Pair<'_, Rule>) -> u64 {
            assert_eq!(pair.as_rule(), Rule::RESULT);
            Self::parse_value(pair.into_inner().next().unwrap()).as_u64()
        }

        fn parse_asm(pair: Pair<'_, Rule>) -> Vec<bpf_insn> {
            assert_eq!(pair.as_rule(), Rule::ASM);
            let mut result: Vec<bpf_insn> = vec![];
            for entry in pair.into_inner() {
                match entry.as_rule() {
                    Rule::ASM_INSTRUCTION => {
                        for instruction in Self::parse_asm_instruction(entry) {
                            result.push(instruction);
                        }
                    }
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            }
            result
        }

        fn parse_asm_instruction(pair: Pair<'_, Rule>) -> Vec<bpf_insn> {
            assert_eq!(pair.as_rule(), Rule::ASM_INSTRUCTION);
            if let Some(entry) = pair.into_inner().next() {
                let mut instruction = bpf_insn::default();
                instruction.code = 0;
                match entry.as_rule() {
                    Rule::ALU_INSTRUCTION => {
                        return vec![Self::parse_alu_instruction(entry)];
                    }
                    Rule::JMP_INSTRUCTION => {
                        return vec![Self::parse_jmp_instruction(entry)];
                    }
                    Rule::MEM_INSTRUCTION => {
                        todo!("Implement mem instructions");
                    }
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            } else {
                vec![]
            }
        }

        fn parse_alu_binary_op(value: &str) -> u8 {
            let mut code: u8 = 0;
            let op = if &value[value.len() - 2..] == "32" {
                code |= BPF_ALU as u8;
                &value[..value.len() - 2]
            } else {
                code |= BPF_ALU64 as u8;
                value
            };
            let alu_op_code = match op {
                "add" => BPF_ADD,
                "sub" => BPF_SUB,
                "mul" => BPF_MUL,
                "div" => BPF_DIV,
                "or" => BPF_OR,
                "and" => BPF_AND,
                "lsh" => BPF_LSH,
                "rsh" => BPF_RSH,
                "mod" => BPF_MOD,
                "xor" => BPF_XOR,
                "mov" => BPF_MOV,
                "arsh" => BPF_ARSH,
                _ => unreachable!("unexpected operation {op}"),
            };
            code |= alu_op_code as u8;
            code
        }

        fn parse_alu_unary_op(value: &str) -> (u8, i32) {
            let (code, imm) = match value {
                "neg" => (BPF_ALU64 | BPF_NEG, 0),
                "neg32" => (BPF_ALU | BPF_NEG, 0),
                "be16" => (BPF_ALU | BPF_SWAP | BPF_REG, 16),
                "be32" => (BPF_ALU | BPF_SWAP | BPF_REG, 32),
                "be64" => (BPF_ALU | BPF_SWAP | BPF_REG, 64),
                "le16" => (BPF_ALU | BPF_SWAP | BPF_IMM, 16),
                "le32" => (BPF_ALU | BPF_SWAP | BPF_IMM, 32),
                "le64" => (BPF_ALU | BPF_SWAP | BPF_IMM, 64),
                _ => unreachable!("unexpected operation {value}"),
            };
            (code as u8, imm)
        }

        fn parse_reg(pair: Pair<'_, Rule>) -> u8 {
            assert_eq!(pair.as_rule(), Rule::REG_NUMBER);
            u8::from_str(&pair.as_str()).expect("parse register")
        }

        fn parse_num(pair: Pair<'_, Rule>) -> u64 {
            assert_eq!(pair.as_rule(), Rule::NUM);
            let num = pair.into_inner().next().unwrap();
            match num.as_rule() {
                Rule::DECNUM => num.as_str().parse().unwrap(),
                Rule::HEXSUFFIX => u64::from_str_radix(num.as_str(), 16).unwrap(),
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
        }

        fn parse_value(pair: Pair<'_, Rule>) -> Value {
            assert!(pair.as_rule() == Rule::IMM || pair.as_rule() == Rule::OFFSET);
            let mut inner = pair.into_inner();
            let mut negative = false;
            let maybe_sign = inner.next().unwrap();
            let num = {
                match maybe_sign.as_rule() {
                    Rule::SIGN => {
                        negative = maybe_sign.as_str() == "-";
                        inner.next().unwrap()
                    }
                    Rule::NUM => maybe_sign,
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            };
            let num = Self::parse_num(num);
            if negative {
                Value::Minus(num)
            } else {
                Value::Plus(num)
            }
        }

        fn parse_src(pair: Pair<'_, Rule>, instruction: &mut bpf_insn) {
            match pair.as_rule() {
                Rule::REG_NUMBER => {
                    instruction.set_src_reg(Self::parse_reg(pair));
                    instruction.code |= BPF_REG as u8;
                }
                Rule::IMM => {
                    instruction.imm = Self::parse_value(pair).as_i32();
                    instruction.code |= BPF_IMM as u8;
                }
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
        }

        fn parse_alu_instruction(pair: Pair<'_, Rule>) -> bpf_insn {
            let mut instruction = bpf_insn::default();
            let mut inner = pair.into_inner();
            let op = inner.next().unwrap();
            match op.as_rule() {
                Rule::BINARY_OP => {
                    instruction.code = Self::parse_alu_binary_op(op.as_str());
                    instruction.set_dst_reg(Self::parse_reg(inner.next().unwrap()));
                    Self::parse_src(inner.next().unwrap(), &mut instruction);
                }
                Rule::UNARY_OP => {
                    instruction.set_dst_reg(Self::parse_reg(inner.next().unwrap()));
                    let (code, imm) = Self::parse_alu_unary_op(op.as_str());
                    instruction.code = code;
                    instruction.imm = imm;
                }
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
            instruction
        }

        fn parse_offset_or_exit(pair: Pair<'_, Rule>) -> i16 {
            match pair.as_rule() {
                Rule::OFFSET => Self::parse_value(pair).as_i16(),
                // This has no equivalent in ebpf. Just ignore.
                Rule::EXIT => 0,
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
        }

        fn parse_jmp_op(value: &str) -> u8 {
            let mut code: u8 = 0;
            // Special case for operation ending by 32 but not being BPF_ALU necessarily
            let op = if &value[value.len() - 2..] == "32" {
                code |= BPF_JMP32 as u8;
                &value[..value.len() - 2]
            } else {
                code |= BPF_JMP as u8;
                value
            };
            let jmp_op_code = match op {
                "jeq" => BPF_JEQ,
                "jgt" => BPF_JGT,
                "jge" => BPF_JGE,
                "jlt" => BPF_JLT,
                "jle" => BPF_JLE,
                "jset" => BPF_JSET,
                "jne" => BPF_JNE,
                "jsgt" => BPF_JSGT,
                "jsge" => BPF_JSGE,
                "jslt" => BPF_JSLT,
                "jsle" => BPF_JSLE,
                _ => unreachable!("unexpected operation {op}"),
            };
            code |= jmp_op_code as u8;
            code
        }
        fn parse_jmp_instruction(pair: Pair<'_, Rule>) -> bpf_insn {
            let mut instruction = bpf_insn::default();
            let mut inner = pair.into_inner();
            let op = inner.next().unwrap();
            match op.as_rule() {
                Rule::JMP_CONDITIONAL => {
                    let mut inner = op.into_inner();
                    instruction.code = Self::parse_jmp_op(inner.next().unwrap().as_str());
                    instruction.set_dst_reg(Self::parse_reg(inner.next().unwrap()));
                    Self::parse_src(inner.next().unwrap(), &mut instruction);
                    instruction.off = Self::parse_offset_or_exit(inner.next().unwrap());
                }
                Rule::JMP => {
                    let mut inner = op.into_inner();
                    instruction.code = (BPF_JMP | BPF_JA) as u8;
                    instruction.off = Self::parse_offset_or_exit(inner.next().unwrap());
                }
                Rule::CALL => {
                    let mut inner = op.into_inner();
                    instruction.code = (BPF_JMP | BPF_CALL) as u8;
                    instruction.imm = Self::parse_value(inner.next().unwrap()).as_i32();
                }
                Rule::EXIT => {
                    instruction.code = (BPF_JMP | BPF_EXIT) as u8;
                }
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
            instruction
        }

        fn parse(content: &str) -> Self {
            let mut pairs =
                TestGrammar::parse(Rule::rules, &content).expect("Parsing must be successful");
            let mut code: Vec<bpf_insn> = vec![];
            let mut result: Option<u64> = None;
            let memory: Vec<u8> = vec![];
            for entry in pairs.next().unwrap().into_inner() {
                match entry.as_rule() {
                    Rule::ASM => {
                        code = Self::parse_asm(entry);
                    }
                    Rule::RESULT => {
                        result = Some(Self::parse_result(entry));
                    }
                    Rule::ERROR => {
                        // Nothing to do.
                    }
                    Rule::EOI => (),
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            }
            TestCase { code, result, memory }
        }
    }

    macro_rules! test_data {
        ($file_name:tt) => {
            include_str!(concat!("../../../../../third_party/ubpf/src/tests/", $file_name))
        };
    }

    #[test_case(test_data!("jle-imm.data"))]
    #[test_case(test_data!("err-infinite-loop.data"))]
    fn test_ebpf_conformance(content: &str) {
        let mut test_case = TestCase::parse(content);
        let builder = EbpfProgramBuilder::new().expect("unable to create builder");
        let program = builder.load(test_case.code, &mut NullVerifierLogger);
        if let Some(value) = test_case.result {
            let result = program
                .expect("program must be loadable")
                .run_with_slice(test_case.memory.as_mut_slice())
                .expect("run");
            assert_eq!(result, value);
        } else {
            assert!(program.is_err());
        }
    }
}
