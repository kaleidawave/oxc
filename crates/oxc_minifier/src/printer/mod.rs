//! Printer with whitespace minification
//! code adapted from [esbuild](https://github.com/evanw/esbuild/blob/main/internal/js_printer/js_printer.go)

#![allow(unused)]

mod gen;
mod operator;

use std::{rc::Rc, str::from_utf8_unchecked};

#[allow(clippy::wildcard_imports)]
use oxc_hir::hir::*;
use oxc_hir::precedence;
use oxc_semantic2::symbol::{SymbolId, SymbolTable};
use oxc_span::Atom;
use oxc_syntax::{
    identifier::is_identifier_part,
    operator::{
        AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
    },
    precedence::Precedence,
};

use self::{
    gen::{Gen, GenExpr},
    operator::Operator,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct PrinterOptions;

pub struct Printer {
    options: PrinterOptions,

    /// Symbol Table for name mangling
    mangle: bool,
    symbol_table: SymbolTable,

    /// Output Code
    code: Vec<u8>,

    // states
    prev_op_end: usize,
    prev_reg_exp_end: usize,
    need_space_before_dot: usize,

    /// For avoiding `;` if the previous statement ends with `}`.
    needs_semicolon: bool,

    prev_op: Option<Operator>,

    start_of_stmt: usize,
    start_of_arrow_expr: usize,
    start_of_default_export: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum Separator {
    Comma,
    Semicolon,
    None,
}

/// Codegen interface for pretty print or minification
impl Printer {
    pub fn new(source_len: usize, options: PrinterOptions) -> Self {
        // Initialize the output code buffer to reduce memory reallocation.
        // Minification will reduce by at least half the original size,
        // so in fact no reallocation should happen at all.
        let capacity = source_len / 2;
        Self {
            options,
            mangle: false,
            symbol_table: SymbolTable::new(),
            code: Vec::with_capacity(capacity),
            needs_semicolon: false,
            need_space_before_dot: 0,
            prev_op_end: 0,
            prev_reg_exp_end: 0,
            prev_op: None,
            start_of_stmt: 0,
            start_of_arrow_expr: 0,
            start_of_default_export: 0,
        }
    }

    #[must_use]
    pub fn with_mangle(mut self, symbol_table: SymbolTable, yes: bool) -> Self {
        if yes {
            self.symbol_table = symbol_table;
            self.mangle = true;
        }
        self
    }

    pub fn build(mut self, program: &Program<'_>) -> String {
        program.gen(&mut self);
        self.into_code()
    }

    fn into_code(self) -> String {
        unsafe { String::from_utf8_unchecked(self.code) }
    }

    fn code(&self) -> &Vec<u8> {
        &self.code
    }

    fn code_len(&self) -> usize {
        self.code().len()
    }

    /// Push a single character into the buffer
    fn print(&mut self, ch: u8) {
        self.code.push(ch);
    }

    /// Push a string into the buffer
    fn print_str(&mut self, s: &[u8]) {
        self.code.extend_from_slice(s);
    }

    fn print_semicolon(&mut self) {
        self.print(b';');
    }

    fn print_comma(&mut self) {
        self.print(b',');
    }

    fn print_space_before_operator(&mut self, next: Operator) {
        if self.prev_op_end != self.code.len() {
            return;
        }
        let Some(prev) = self.prev_op else { return };
        // "+ + y" => "+ +y"
        // "+ ++ y" => "+ ++y"
        // "x + + y" => "x+ +y"
        // "x ++ + y" => "x+++y"
        // "x + ++ y" => "x+ ++y"
        // "-- >" => "-- >"
        // "< ! --" => "<! --"
        let bin_op_add = Operator::Binary(BinaryOperator::Addition);
        let bin_op_sub = Operator::Binary(BinaryOperator::Subtraction);
        let un_op_pos = Operator::Unary(UnaryOperator::UnaryPlus);
        let un_op_pre_inc = Operator::Update(UpdateOperator::Increment);
        let un_op_neg = Operator::Unary(UnaryOperator::UnaryNegation);
        let un_op_pre_dec = Operator::Update(UpdateOperator::Decrement);
        let un_op_post_dec = Operator::Update(UpdateOperator::Decrement);
        let bin_op_gt = Operator::Binary(BinaryOperator::GreaterThan);
        let un_op_not = Operator::Unary(UnaryOperator::LogicalNot);
        if ((prev == bin_op_add || prev == un_op_pos)
            && (next == bin_op_add || next == un_op_pos || next == un_op_pre_inc))
            || ((prev == bin_op_sub || prev == un_op_neg)
                && (next == bin_op_sub || next == un_op_neg || next == un_op_pre_dec))
            || (prev == un_op_post_dec && next == bin_op_gt)
            || (prev == un_op_not && next == un_op_pre_dec && self.peek_nth(1) == Some('<'))
        {
            self.print(b' ');
        }
    }

    fn print_space_before_identifier(&mut self) {
        let ch = self.peek_nth(0);

        if let Some(ch) = ch && (
            is_identifier_part(ch)
            || self.prev_reg_exp_end == self.code.len()
         ) {
            self.print(b' ');
        }
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        unsafe { from_utf8_unchecked(self.code()) }.chars().nth_back(n)
    }

    fn print_semicolon_after_statement(&mut self) {
        self.needs_semicolon = true;
    }

    fn print_semicolon_if_needed(&mut self) {
        if self.needs_semicolon {
            self.print_semicolon();
            self.needs_semicolon = false;
        }
    }

    fn print_ellipsis(&mut self) {
        self.print_str(b"...");
    }

    fn print_colon(&mut self) {
        self.print(b':');
    }

    fn print_equal(&mut self) {
        self.print(b'=');
    }

    fn print_sequence<T: Gen>(&mut self, items: &[T], separator: Separator) {
        let len = items.len();
        for (index, item) in items.iter().enumerate() {
            item.gen(self);
            match separator {
                Separator::Semicolon => self.print_semicolon(),
                Separator::Comma => self.print(b','),
                Separator::None => {}
            }
            if index != len - 1 {}
        }
    }

    fn print_body(&mut self, stmt: &Statement<'_>) {
        if let Statement::BlockStatement(block) = stmt {
            self.print_block1(block);
        } else {
            stmt.gen(self);
        }
    }

    fn print_block1(&mut self, stmt: &BlockStatement<'_>) {
        self.print(b'{');
        for item in &stmt.body {
            self.print_semicolon_if_needed();
            item.gen(self);
        }
        self.needs_semicolon = false;
        self.print(b'}');
    }

    fn print_block<T: Gen>(&mut self, items: &[T], separator: Separator) {
        self.print(b'{');
        if !items.is_empty() {}
        self.print_sequence(items, separator);
        if !items.is_empty() {}
        self.print(b'}');
    }

    fn print_list<T: Gen>(&mut self, items: &[T]) {
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.print_comma();
            }
            item.gen(self);
        }
    }

    fn print_expressions<T: GenExpr>(&mut self, items: &[T], precedence: Precedence) {
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.print_comma();
            }
            item.gen_expr(self, precedence);
        }
    }

    fn print_symbol(&mut self, symbol_id: SymbolId, fallback: &Atom) {
        if self.mangle {
            let name = self.symbol_table.get_name(symbol_id).clone();
            self.print_str(name.as_bytes());
        } else {
            self.print_str(fallback.as_bytes());
        }
    }

    fn wrap<F: FnMut(&mut Self)>(&mut self, wrap: bool, mut f: F) {
        if wrap {
            self.print(b'(');
        }
        f(self);
        if wrap {
            self.print(b')');
        }
    }
}
