//! Oxc Codegen
//!
//! Supports
//!
//! * whitespace removal
//! * sourcemaps
//!
//! Code adapted from
//! * [esbuild](https://github.com/evanw/esbuild/blob/main/internal/js_printer/js_printer.go)

mod context;
mod gen;
mod operator;

use std::str::from_utf8_unchecked;

#[allow(clippy::wildcard_imports)]
use oxc_ast::ast::*;
use oxc_span::Atom;
use oxc_syntax::{
    identifier::is_identifier_part,
    operator::{BinaryOperator, UnaryOperator, UpdateOperator},
    precedence::Precedence,
    symbol::SymbolId,
};

pub use crate::{
    context::Context,
    gen::{Gen, GenExpr},
    operator::Operator,
};
// use crate::mangler::Mangler;

#[derive(Debug, Default, Clone, Copy)]
pub struct CodegenOptions;

pub struct Codegen<const MINIFY: bool> {
    #[allow(unused)]
    options: CodegenOptions,

    // mangler: Option<Mangler>,
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

    /// Track the current indentation level
    indentation: u8,
}

#[derive(Debug, Clone, Copy)]
pub enum Separator {
    Comma,
    Semicolon,
    None,
}

impl<const MINIFY: bool> Codegen<MINIFY> {
    pub fn new(source_len: usize, options: CodegenOptions) -> Self {
        // Initialize the output code buffer to reduce memory reallocation.
        // Minification will reduce by at least half of the original size.
        let capacity = if MINIFY { source_len / 2 } else { source_len };
        Self {
            options,
            // mangler: None,
            code: Vec::with_capacity(capacity),
            needs_semicolon: false,
            need_space_before_dot: 0,
            prev_op_end: 0,
            prev_reg_exp_end: 0,
            prev_op: None,
            start_of_stmt: 0,
            start_of_arrow_expr: 0,
            start_of_default_export: 0,
            indentation: 0,
        }
    }

    // fn with_mangler(&mut self, mangler: Mangler) {
    // self.mangler = Some(mangler);
    // }

    pub fn build(mut self, program: &Program<'_>) -> String {
        program.gen(&mut self, Context::default());
        self.into_code()
    }

    pub fn into_code(self) -> String {
        // SAFETY: criteria of `from_utf8_unchecked`.are met.
        unsafe { String::from_utf8_unchecked(self.code) }
    }

    fn code(&self) -> &Vec<u8> {
        &self.code
    }

    fn code_len(&self) -> usize {
        self.code().len()
    }

    /// Push a single character into the buffer
    pub fn print(&mut self, ch: u8) {
        self.code.push(ch);
    }

    /// Push a string into the buffer
    pub fn print_str(&mut self, s: &[u8]) {
        self.code.extend_from_slice(s);
    }

    fn print_soft_space(&mut self) {
        if !MINIFY {
            self.print(b' ');
        }
    }

    pub fn print_hard_space(&mut self) {
        self.print(b' ');
    }

    fn print_soft_newline(&mut self) {
        if !MINIFY {
            self.print(b'\n');
        }
    }

    fn print_semicolon(&mut self) {
        self.print(b';');
    }

    fn print_comma(&mut self) {
        self.print(b',');
    }

    fn print_space_before_identifier(&mut self) {
        if self
            .peek_nth(0)
            .is_some_and(|ch| is_identifier_part(ch) || self.prev_reg_exp_end == self.code.len())
        {
            self.print_hard_space();
        }
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        // SAFETY: criteria of `from_utf8_unchecked`.are met.
        unsafe { from_utf8_unchecked(self.code()) }.chars().nth_back(n)
    }

    fn indent(&mut self) {
        if !MINIFY {
            self.indentation += 1;
        }
    }

    fn dedent(&mut self) {
        if !MINIFY {
            self.indentation -= 1;
        }
    }

    fn print_indent(&mut self) {
        if !MINIFY {
            for _ in 0..self.indentation {
                self.print(b'\t');
            }
        }
    }

    fn print_semicolon_after_statement(&mut self) {
        if MINIFY {
            self.needs_semicolon = true;
        } else {
            self.print_str(b";\n");
        }
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

    pub fn print_colon(&mut self) {
        self.print(b':');
    }

    fn print_equal(&mut self) {
        self.print(b'=');
    }

    fn print_sequence<T: Gen<MINIFY>>(&mut self, items: &[T], separator: Separator, ctx: Context) {
        let len = items.len();
        for (index, item) in items.iter().enumerate() {
            item.gen(self, ctx);
            match separator {
                Separator::Semicolon => self.print_semicolon(),
                Separator::Comma => self.print(b','),
                Separator::None => {}
            }
            if index != len - 1 {}
        }
    }

    fn print_block_start(&mut self) {
        self.print(b'{');
        self.print_soft_newline();
        self.indent();
    }

    fn print_block_end(&mut self) {
        self.dedent();
        self.print_indent();
        self.print(b'}');
    }

    fn print_block1(&mut self, stmt: &BlockStatement<'_>, ctx: Context) {
        self.print_block_start();
        for item in &stmt.body {
            self.print_semicolon_if_needed();
            item.gen(self, ctx);
        }
        self.print_block_end();
        self.needs_semicolon = false;
    }

    fn print_block<T: Gen<MINIFY>>(&mut self, items: &[T], separator: Separator, ctx: Context) {
        self.print_block_start();
        self.print_sequence(items, separator, ctx);
        self.print_block_end();
    }

    fn print_list<T: Gen<MINIFY>>(&mut self, items: &[T], ctx: Context) {
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.print_comma();
                self.print_soft_space();
            }
            item.gen(self, ctx);
        }
    }

    pub fn print_expression(&mut self, expr: &Expression<'_>) {
        expr.gen_expr(self, Precedence::lowest(), Context::default());
    }

    fn print_expressions<T: GenExpr<MINIFY>>(
        &mut self,
        items: &[T],
        precedence: Precedence,
        ctx: Context,
    ) {
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.print_comma();
            }
            item.gen_expr(self, precedence, ctx);
        }
    }

    fn print_symbol(&mut self, _symbol_id: Option<SymbolId>, fallback: &Atom) {
        // if let Some(mangler) = &self.mangler {
        // if let Some(symbol_id) = symbol_id {
        // let name = mangler.get_symbol_name(symbol_id);
        // self.print_str(name.clone().as_bytes());
        // return;
        // }
        // }
        self.print_str(fallback.as_bytes());
    }

    fn print_space_before_operator(&mut self, next: Operator) {
        if !MINIFY {
            self.print_hard_space();
            return;
        }
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
            self.print_hard_space();
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
