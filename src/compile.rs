#![allow(visible_private_types)]

use std::cmp;
use std::iter;
use std::slice::Vector;
use std::str::{MaybeOwned, Owned};
use super::parse;
use super::parse::{Nothing, Literal, Dot, Class, Begin, End, WordBoundary};
use super::parse::{Capture, Cat, Alt, Rep};
use super::parse::{ZeroOne, ZeroMore, OneMore};

type InstIdx = uint;

#[deriving(Show, Clone)]
pub enum MaybeStaticClass {
    DynamicClass(Vec<(char, char)>),
    StaticClass(&'static [(char, char)]),
}

impl Vector<(char, char)> for MaybeStaticClass {
    fn as_slice<'a>(&'a self) -> &'a [(char, char)] {
        match *self {
            DynamicClass(ref xs) => xs.as_slice(),
            StaticClass(xs) => xs,
        }
    }
}

#[deriving(Show, Clone)]
pub enum Inst {
    // When a Match instruction is executed, the current thread is successful.
    Match,

    // The Char instruction matches a literal character.
    // If the bool is true, then the match is done case insensitively.
    Char_(char, bool),

    // The CharClass instruction tries to match one input character against
    // the range of characters given.
    // If the first bool is true, then the character class is negated.
    // If the second bool is true, then the character class is matched
    // case insensitively.
    CharClass(MaybeStaticClass, bool, bool),

    // Matches any character except new lines.
    // If the bool is true, then new lines are matched.
    Any_(bool),

    // Matches the beginning of the string, consumes no characters.
    // If the bool is true, then it also matches when the preceding character
    // is a new line.
    EmptyBegin(bool),

    // Matches the end of the string, consumes no characters.
    // If the bool is true, then it also matches when the proceeding character
    // is a new line.
    EmptyEnd(bool),

    // Matches a word boundary (\w on one side and \W \A or \z on the other),
    // and consumes no character.
    // If the bool is false, then it matches anything that is NOT a word
    // boundary.
    EmptyWordBoundary(bool),

    // Saves the current position in the input string to the Nth save slot.
    Save(uint),

    // Jumps to the instruction at the index given.
    Jump(InstIdx),

    // Jumps to the instruction at the first index given. If that leads to
    // a failing state, then the instruction at the second index given is
    // tried.
    Split(InstIdx, InstIdx),
}

pub trait Program<'r> {
    fn regex(&'r self) -> &'r str;
    fn insts(&'r self) -> &'r [Inst];
    fn names(&'r self) -> &'r [Option<MaybeOwned<'r>>];
    fn prefix(&'r self) -> &'r [char];

    fn num_captures(&'r self) -> uint {
        let mut n = 0;
        for inst in self.insts().iter() {
            match *inst {
                Save(c) => n = cmp::max(n, c+1),
                _ => {}
            }
        }
        // There's exactly 2 Save slots for every capture.
        n / 2
    }
}

pub struct StaticProgram {
    pub regex: &'static str,
    pub insts: &'static [Inst],
    pub names: &'static [Option<MaybeOwned<'static>>],
    pub prefix: &'static [char],
}

impl Program<'static> for &'static StaticProgram {
    fn regex(&'static self) -> &'static str { self.regex }
    fn insts(&'static self) -> &'static [Inst] { self.insts }
    fn names(&'static self) -> &'static [Option<MaybeOwned<'static>>] { self.names }
    fn prefix(&'static self) -> &'static [char] { self.prefix }
}

pub struct DynamicProgram {
    pub regex: ~str,
    pub insts: Vec<Inst>,
    pub names: Vec<Option<MaybeOwned<'static>>>,
    pub prefix: Vec<char>,
}

impl<'r> Program<'r> for DynamicProgram {
    fn regex(&'r self) -> &'r str { self.regex.as_slice() }
    fn insts(&'r self) -> &'r [Inst] { self.insts.as_slice() }
    fn names(&'r self) -> &'r [Option<MaybeOwned<'static>>] { self.names.as_slice() }
    fn prefix(&'r self) -> &'r [char] { self.prefix.as_slice() }
}

impl DynamicProgram {
    pub fn new(regex: &str, ast: ~parse::Ast) -> DynamicProgram {
        let mut c = Compiler {
            insts: Vec::with_capacity(100),
            names: Vec::with_capacity(10),
        };

        c.insts.push(Save(0));
        c.compile(ast);
        c.insts.push(Save(1));
        c.insts.push(Match);

        // Try to discover a literal string prefix.
        // This is a bit hacky since we have to skip over the initial
        // 'Save' instruction.
        let mut pre = Vec::with_capacity(5);
        for i in iter::range(1, c.insts.len()) {
            match *c.insts.get(i) {
                Char_(c, false) => pre.push(c),
                _ => break
            }
        }

        let names = c.names.clone();
        DynamicProgram {
            regex: regex.to_owned(),
            insts: c.insts,
            names: names,
            prefix: pre,
        }
    }
}

struct Compiler<'r> {
    insts: Vec<Inst>,
    names: Vec<Option<MaybeOwned<'r>>>,
}

impl<'r> Compiler<'r> {
    fn compile(&mut self, ast: ~parse::Ast) {
        match ast {
            ~Nothing => {},
            ~Literal(c, casei) => self.push(Char_(c, casei)),
            ~Dot(nl) => self.push(Any_(nl)),
            ~Class(ranges, negated, casei) =>
                self.push(CharClass(DynamicClass(ranges), negated, casei)),
            ~Begin(multi) => self.push(EmptyBegin(multi)),
            ~End(multi) => self.push(EmptyEnd(multi)),
            ~WordBoundary(yes) => self.push(EmptyWordBoundary(yes)),
            ~Capture(cap, name, x) => {
                let len = self.names.len();
                if cap >= len {
                    self.names.grow(10 + cap - len, &None)
                }
                *self.names.get_mut(cap) = name.map(Owned);

                self.push(Save(2 * cap));
                self.compile(x);
                self.push(Save(2 * cap + 1));
            }
            ~Cat(xs) => {
                for x in xs.move_iter() {
                    self.compile(x)
                }
            }
            ~Alt(x, y) => {
                let split = self.empty_split(); // push: split 0, 0
                let j1 = self.insts.len();
                self.compile(x);                // push: insts for x
                let jmp = self.empty_jump();    // push: jmp 0
                let j2 = self.insts.len();
                self.compile(y);                // push: insts for y
                let j3 = self.insts.len();

                self.set_split(split, j1, j2);  // split 0, 0 -> split j1, j2
                self.set_jump(jmp, j3);         // jmp 0      -> jmp j3
            }
            ~Rep(x, ZeroOne, g) => {
                let split = self.empty_split();
                let j1 = self.insts.len();
                self.compile(x);
                let j2 = self.insts.len();

                if g.is_greedy() {
                    self.set_split(split, j1, j2);
                } else {
                    self.set_split(split, j2, j1);
                }
            }
            ~Rep(x, ZeroMore, g) => {
                let j1 = self.insts.len();
                let split = self.empty_split();
                let j2 = self.insts.len();
                self.compile(x);
                let jmp = self.empty_jump();
                let j3 = self.insts.len();

                self.set_jump(jmp, j1);
                if g.is_greedy() {
                    self.set_split(split, j2, j3);
                } else {
                    self.set_split(split, j3, j2);
                }
            }
            ~Rep(x, OneMore, g) => {
                let j1 = self.insts.len();
                self.compile(x);
                let split = self.empty_split();
                let j2 = self.insts.len();

                if g.is_greedy() {
                    self.set_split(split, j1, j2);
                } else {
                    self.set_split(split, j2, j1);
                }
            }
        }
    }

    #[inline(always)]
    fn push(&mut self, x: Inst) {
        self.insts.push(x)
    }

    #[inline(always)]
    fn empty_split(&mut self) -> InstIdx {
        self.insts.push(Split(0, 0));
        self.insts.len() - 1
    }

    #[inline(always)]
    fn set_split(&mut self, i: InstIdx, pc1: InstIdx, pc2: InstIdx) {
        let split = self.insts.get_mut(i);
        match *split {
            Split(_, _) => *split = Split(pc1, pc2),
            _ => fail!("BUG: Invalid split index."),
        }
    }

    #[inline(always)]
    fn empty_jump(&mut self) -> InstIdx {
        self.insts.push(Jump(0));
        self.insts.len() - 1
    }

    #[inline(always)]
    fn set_jump(&mut self, i: InstIdx, pc: InstIdx) {
        let jmp = self.insts.get_mut(i);
        match *jmp {
            Jump(_) => *jmp = Jump(pc),
            _ => fail!("BUG: Invalid jump index."),
        }
    }
}
