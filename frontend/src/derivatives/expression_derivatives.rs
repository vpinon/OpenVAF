//  * ******************************************************************************************
//  * Copyright (c) 2020 Pascal Kuthe. This file is part of the frontend project.
//  * It is subject to the license terms in the LICENSE file found in the top-level directory
//  *  of this distribution and at  https://gitlab.com/DSPOM/OpenVAF/blob/master/LICENSE.
//  *  No part of frontend, including this file, may be copied, modified, propagated, or
//  *  distributed except according to the terms contained in the LICENSE file.
//  * *******************************************************************************************

use crate::ast::UnaryOperator;
use crate::derivatives::error::Error::{DerivativeNotDefined, OnlyNumericExpressionsCanBeDerived};
use crate::derivatives::error::UndefinedDerivative;
use crate::derivatives::lints::RoundingDerivativeNotFullyDefined;
use crate::derivatives::{AutoDiff, Unknown};
use crate::hir::{Branch, DisciplineAccess};
use crate::ir::mir::visit::integer_expressions::{
    IntegerBinaryOperatorVisitor, IntegerExprVisitor,
};
use crate::ir::mir::visit::real_expressions::{
    walk_real_expression, RealBuiltInFunctionCall2pVisitor, RealExprVisitor,
};
use crate::ir::mir::RealBinaryOperator::{Divide, Multiply, Subtract, Sum};
use crate::ir::mir::RealExpression::{BranchAccess, IntegerConversion};
use crate::ir::mir::{ExpressionId, Mir};
use crate::ir::BuiltInFunctionCall1p::{Cos, CosH, Ln, Sin, SinH, Sqrt};
use crate::ir::{
    BranchId, BuiltInFunctionCall1p, BuiltInFunctionCall2p, IntegerExpressionId, NetId, Node,
    NoiseSource, ParameterId, PortId, RealExpressionId, StringExpressionId, VariableId,
};
use crate::lints::dispatch_late;
use crate::mir::visit::integer_expressions::walk_integer_expression;
use crate::mir::visit::real_expressions::{
    RealBinaryOperatorVisitor, RealBuiltInFunctionCall1pVisitor,
};
use crate::mir::{
    ComparisonOperator, IntegerBinaryOperator, IntegerExpression, RealBinaryOperator,
    RealExpression,
};
use crate::sourcemap::span::DUMMY_SP;
use crate::{Span, StringLiteral};

pub struct ExpressionAutoDiff<'lt, 'mir: 'lt, E> {
    current_expr: E,
    ad: &'lt mut AutoDiff<'mir>,
    unknown: Unknown,
}

/// Inside this module Nonde reprents a derivative that evaluates to 0
type Derivative = Option<RealExpressionId>;

impl<'lt, 'mir: 'lt, E: Into<ExpressionId> + Copy> ExpressionAutoDiff<'lt, 'mir, E> {
    fn add_to_mir(&mut self, expr: RealExpression) -> RealExpressionId {
        let node = Node::new(expr, self.current_expr.into().span(self.ad.mir));
        self.ad.mir.real_expressions.push(node)
    }

    fn gen_constant(&mut self, val: f64) -> RealExpressionId {
        let expr = RealExpression::Literal(val);
        self.add_to_mir(expr)
    }

    fn gen_int_constant(&mut self, val: i64) -> IntegerExpressionId {
        let expr = IntegerExpression::Literal(val);
        let node = Node::new(expr, self.current_expr.into().span(self.ad.mir));
        self.ad.mir.integer_expressions.push(node)
    }

    fn gen_neg(&mut self, expr: RealExpressionId) -> RealExpressionId {
        let span = self.current_expr.into().span(self.ad.mir);
        let expr = RealExpression::Negate(span, expr);
        self.add_to_mir(expr)
    }

    fn gen_binary_op(
        &mut self,
        lhs: RealExpressionId,
        op: RealBinaryOperator,
        rhs: RealExpressionId,
    ) -> RealExpressionId {
        let span = self.current_expr.into().span(self.ad.mir);
        let expr = RealExpression::BinaryOperator(lhs, Node::new(op, span), rhs);
        self.add_to_mir(expr)
    }

    fn gen_math_function(
        &mut self,
        call: BuiltInFunctionCall1p,
        arg: RealExpressionId,
    ) -> RealExpressionId {
        let expr = RealExpression::BuiltInFunctionCall1p(call, arg);
        self.add_to_mir(expr)
    }

    fn gen_one_plus_minus_squared(
        &mut self,
        minus: bool,
        arg: RealExpressionId,
    ) -> RealExpressionId {
        let one = self.gen_constant(1.0);
        let sqare = self.gen_binary_op(arg, Multiply, arg);
        let op = if minus { Subtract } else { Sum };
        self.gen_binary_op(one, op, sqare)
    }

    /// # Returns
    ///
    ///  * `None` — if `arg1` and `arg2` are `None`
    /// * One argument and 0 — if one is `None` but the other isn't
    /// * the expressions of arg1 and arg2 — if both are something
    ///
    /// # Note
    ///
    /// The order of arg1 and arg2 is preserved in the return argument
    ///
    fn convert_to_paired(
        &mut self,
        arg1: Derivative,
        arg2: Derivative,
    ) -> Option<(RealExpressionId, RealExpressionId)> {
        let (arg1, arg2) = match (arg1, arg2) {
            (Some(arg1), Some(arg2)) => (arg1, arg2),
            (Some(arg1), None) => (arg1, self.gen_constant(0.0)),
            (None, Some(arg2)) => (self.gen_constant(0.0), arg2),
            (None, None) => return None,
        };
        Some((arg1, arg2))
    }

    fn derivative_sum(&mut self, dlhs: Derivative, drhs: Derivative) -> Derivative {
        match (dlhs, drhs) {
            (Some(dlhs), Some(drhs)) => Some(self.gen_binary_op(dlhs, Sum, drhs)),
            (Some(res), None) | (None, Some(res)) => Some(res),
            (None, None) => None,
        }
    }

    fn mul_derivative(
        &mut self,
        lhs: RealExpressionId,
        dlhs: Derivative,
        rhs: RealExpressionId,
        drhs: Derivative,
    ) -> Derivative {
        // u = a'*b
        let factor1 = if let Some(dlhs) = dlhs {
            Some(self.gen_binary_op(dlhs, Multiply, rhs))
        } else {
            None
        };

        // v = a*b'
        let factor2 = if let Some(drhs) = drhs {
            Some(self.gen_binary_op(lhs, Multiply, drhs))
        } else {
            None
        };

        // u+v
        self.derivative_sum(factor1, factor2)
    }

    fn quotient_derivative(
        &mut self,
        lhs: RealExpressionId,
        dlhs: Derivative,
        rhs: RealExpressionId,
        drhs: Derivative,
    ) -> Derivative {
        let drhs = drhs.map(|drhs| self.gen_neg(drhs));

        // num = u'*v+v'*u (u=lhs, v=rhs, derivatives are calculated above)
        let num = self.mul_derivative(lhs, dlhs, rhs, drhs)?;

        // den = g*g
        let den = self.gen_binary_op(rhs, Multiply, rhs);

        // (f/g)' = num/den = (f'*g+(-g')*f)/g*g
        Some(self.gen_binary_op(num, Divide, den))
    }

    fn pow_derivative(
        &mut self,
        lhs: RealExpressionId,
        dlhs: Derivative,
        rhs: RealExpressionId,
        drhs: Derivative,
        original: RealExpressionId,
    ) -> Derivative {
        // rhs/lhs * lhs'
        let sum1 = if let Some(dlhs) = dlhs {
            let quotient = self.gen_binary_op(rhs, Divide, lhs);
            Some(self.gen_binary_op(quotient, Multiply, dlhs))
        } else {
            None
        };

        //ln (lhs) * rhs'
        let sum2 = if let Some(drhs) = drhs {
            let ln = self.gen_math_function(Ln, lhs);
            Some(self.gen_binary_op(ln, Multiply, drhs))
        } else {
            None
        };

        // f'/f*g + ln(f)*g'
        let sum = self.derivative_sum(sum1, sum2)?;

        // (f**g)' = sum* f**g = (f'/f*g + ln(f)*g')* f**g
        Some(self.gen_binary_op(sum, Multiply, original))
    }

    fn undefined_derivative(&mut self, undefined: UndefinedDerivative) {
        let span = self.current_expr.into().span(self.ad.mir);
        self.ad.errors.add(DerivativeNotDefined(undefined, span))
    }

    fn param_derivative(&mut self, param: ParameterId) -> Derivative {
        if Unknown::Parameter(param) == self.unknown {
            Some(self.gen_constant(1.0))
        } else {
            None
        }
    }
}

impl<'lt, 'mir: 'lt> ExpressionAutoDiff<'lt, 'mir, RealExpressionId> {
    /// Generates arg1 < arg2
    fn gen_lt_condition(
        &mut self,
        arg1: RealExpressionId,
        arg2: RealExpressionId,
    ) -> IntegerExpressionId {
        let span = self.ad.mir[self.current_expr].span;
        let condition = IntegerExpression::RealComparison(
            arg1,
            Node::new(ComparisonOperator::LessThen, span),
            arg2,
        );
        let condition = Node::new(condition, span);
        self.ad.mir.integer_expressions.push(condition)
    }
    pub fn run(&mut self) -> RealExpressionId {
        self.visit_real_expr(self.current_expr)
            .unwrap_or_else(|| self.gen_constant(0.0))
    }
}

impl<'lt, 'mir: 'lt> ExpressionAutoDiff<'lt, 'mir, IntegerExpressionId> {
    /// Generates arg1 < arg2
    fn gen_lt_condition(
        &mut self,
        arg1: IntegerExpressionId,
        arg2: IntegerExpressionId,
    ) -> IntegerExpressionId {
        let span = self.ad.mir[self.current_expr].span;
        let condition = IntegerExpression::IntegerComparison(
            arg1,
            Node::new(ComparisonOperator::LessThen, span),
            arg2,
        );
        let condition = Node::new(condition, span);
        self.ad.mir.integer_expressions.push(condition)
    }

    pub fn run(&mut self) -> RealExpressionId {
        self.visit_integer_expr(self.current_expr)
            .unwrap_or_else(|| self.gen_constant(0.0))
    }
}

impl<'lt, 'mir: 'lt> RealExprVisitor for ExpressionAutoDiff<'lt, 'mir, RealExpressionId> {
    type T = Derivative;

    #[inline]
    fn visit_real_expr(&mut self, expr: RealExpressionId) -> Derivative {
        let old = self.current_expr;
        self.current_expr = expr;
        let res = walk_real_expression(self, expr);
        self.current_expr = old;
        res
    }

    fn mir(&self) -> &Mir {
        self.ad.mir
    }

    fn visit_literal(&mut self, _val: f64) -> Derivative {
        None
    }

    fn visit_binary_operator(
        &mut self,
        lhs: RealExpressionId,
        op: Node<RealBinaryOperator>,
        rhs: RealExpressionId,
    ) -> Derivative {
        self.visit_real_binary_op(lhs, op.contents, rhs)
    }

    fn visit_builtin_function_call_1p(
        &mut self,
        call: BuiltInFunctionCall1p,
        arg: RealExpressionId,
    ) -> Derivative {
        RealBuiltInFunctionCall1pVisitor::visit_real_builtin_function_call_1p(self, call, arg)
    }

    fn visit_builtin_function_call_2p(
        &mut self,
        call: BuiltInFunctionCall2p,
        arg1: RealExpressionId,
        arg2: RealExpressionId,
    ) -> Derivative {
        RealBuiltInFunctionCall2pVisitor::visit_real_builtin_function_call_2p(
            self, call, arg1, arg2,
        )
    }

    fn visit_negate(&mut self, op: Span, arg: RealExpressionId) -> Derivative {
        let arg = self.visit_real_expr(arg)?;
        Some(self.add_to_mir(RealExpression::Negate(op, arg)))
    }

    fn visit_condition(
        &mut self,
        cond: IntegerExpressionId,
        true_expr: RealExpressionId,
        false_expr: RealExpressionId,
    ) -> Derivative {
        let true_expr = self.visit_real_expr(true_expr);
        let false_expr = self.visit_real_expr(false_expr);
        let (true_expr, false_expr) = self.convert_to_paired(true_expr, false_expr)?;
        let expr = RealExpression::Condition(cond, true_expr, false_expr);
        Some(self.add_to_mir(expr))
    }

    fn visit_variable_reference(&mut self, var: VariableId) -> Derivative {
        let var = self.ad.mir.derivative_var(var, self.unknown);
        Some(self.add_to_mir(RealExpression::VariableReference(var)))
    }

    fn visit_parameter_reference(&mut self, param: ParameterId) -> Derivative {
        self.param_derivative(param)
    }

    fn visit_branch_access(
        &mut self,
        discipline_accesss: DisciplineAccess,
        branch: BranchId,
        time_derivative_order: u8,
    ) -> Derivative {
        match self.unknown {
            Unknown::Time => Some(self.add_to_mir(BranchAccess(
                discipline_accesss,
                branch,
                time_derivative_order + 1,
            ))),
            Unknown::NodePotential(net) => match self.mir()[branch].contents.branch {
                Branch::Nets(uppper, _) if uppper == net => Some(self.gen_constant(1.0)),
                Branch::Nets(_, lower) if lower == net => Some(self.gen_constant(-1.0)),
                _ => None,
            },
            Unknown::Flow(unknown) if unknown == branch => Some(self.gen_constant(1.0)),
            _ => None,
        }
    }

    fn visit_noise(
        &mut self,
        _noise_src: NoiseSource<RealExpressionId, ()>,
        _name: Option<StringLiteral>,
    ) -> Derivative {
        // TODO Warn
        None
    }

    fn visit_temperature(&mut self) -> Derivative {
        if self.unknown == Unknown::Temperature {
            Some(self.gen_constant(1.0))
        } else {
            None
        }
    }

    fn visit_sim_param(
        &mut self,
        _name: StringExpressionId,
        _default: Option<RealExpressionId>,
    ) -> Derivative {
        None
    }

    fn visit_integer_conversion(&mut self, expr: IntegerExpressionId) -> Derivative {
        ExpressionAutoDiff {
            current_expr: expr,
            unknown: self.unknown,
            ad: self.ad,
        }
        .visit_integer_expr(expr)
    }
}

impl<'lt, 'mir: 'lt> RealBinaryOperatorVisitor for ExpressionAutoDiff<'lt, 'mir, RealExpressionId> {
    type T = Derivative;

    fn visit_sum(&mut self, lhs: RealExpressionId, rhs: RealExpressionId) -> Derivative {
        let dlhs = self.visit_real_expr(lhs);
        let drhs = self.visit_real_expr(rhs);
        self.derivative_sum(dlhs, drhs)
    }

    fn visit_diff(&mut self, lhs: RealExpressionId, rhs: RealExpressionId) -> Derivative {
        let dlhs = self.visit_real_expr(lhs);
        let drhs = self.visit_real_expr(rhs);
        let drhs = drhs.map(|drhs| self.gen_neg(drhs));
        self.derivative_sum(dlhs, drhs)
    }

    fn visit_mul(&mut self, lhs: RealExpressionId, rhs: RealExpressionId) -> Derivative {
        let dlhs = self.visit_real_expr(lhs);
        let drhs = self.visit_real_expr(rhs);
        self.mul_derivative(lhs, dlhs, rhs, drhs)
    }

    fn visit_quotient(&mut self, lhs: RealExpressionId, rhs: RealExpressionId) -> Derivative {
        let dlhs = self.visit_real_expr(lhs);
        let drhs = self.visit_real_expr(rhs);

        self.quotient_derivative(lhs, dlhs, rhs, drhs)
    }

    fn visit_pow(&mut self, lhs: RealExpressionId, rhs: RealExpressionId) -> Derivative {
        let dlhs = self.visit_real_expr(lhs);
        let drhs = self.visit_real_expr(rhs);

        self.pow_derivative(lhs, dlhs, rhs, drhs, self.current_expr)
    }

    fn visit_mod(&mut self, _lhs: RealExpressionId, _rhs: RealExpressionId) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::Modulus);
        None
    }
}

impl<'lt, 'mir: 'lt> RealBuiltInFunctionCall2pVisitor
    for ExpressionAutoDiff<'lt, 'mir, RealExpressionId>
{
    type T = Derivative;

    fn visit_pow(&mut self, arg1: RealExpressionId, arg2: RealExpressionId) -> Derivative {
        // a**b is the same as pow(a,b)
        RealBinaryOperatorVisitor::visit_pow(self, arg1, arg2)
    }

    fn visit_hypot(&mut self, arg1: RealExpressionId, arg2: RealExpressionId) -> Derivative {
        let darg1 = self.visit_real_expr(arg1);
        let darg2 = self.visit_real_expr(arg2);

        // arguments swapped to get arg2*darg2+ar1*darg1 instead of arg1*darg2+arg2*darg1
        let num = self.mul_derivative(arg2, darg1, arg1, darg2)?;

        // ( hypport(f,g) )' = ( f * f' + g * g' ) /  hyppot(f,g)
        Some(self.gen_binary_op(num, Divide, self.current_expr))
    }

    fn visit_arctan2(&mut self, arg1: RealExpressionId, arg2: RealExpressionId) -> Derivative {
        // u' = f'
        let darg1 = self.visit_real_expr(arg1);

        // v' = -g'
        let darg2 = self.visit_real_expr(arg2);
        let darg2 = darg2.map(|darg2| self.gen_neg(darg2));

        // num = u'*v+v'*u (u=lhs, v=rhs, derivatives are calculated above)
        let num = self.mul_derivative(arg1, darg1, arg2, darg2)?;

        // den = g*g + f*f
        let sum1 = self.gen_binary_op(arg1, Multiply, arg1);
        let sum2 = self.gen_binary_op(arg2, Multiply, arg2);
        let den = self.gen_binary_op(sum1, Sum, sum2);

        // ( arctan2(f,g) )' = num/den = (f'g - g'*f)/(f^2+g^2)
        Some(self.gen_binary_op(num, Divide, den))
    }

    fn visit_max(&mut self, arg1: RealExpressionId, arg2: RealExpressionId) -> Derivative {
        // arg2 < arg1
        let condition = self.gen_lt_condition(arg2, arg1);
        // max = if (arg2 < arg1) arg1 else arg2
        // this generates the derivative of that
        self.visit_condition(condition, arg1, arg2)
    }

    fn visit_min(&mut self, arg1: RealExpressionId, arg2: RealExpressionId) -> Derivative {
        // arg1 < arg2
        let condition = self.gen_lt_condition(arg1, arg2);
        // max = if (arg1 < arg2) arg1 else arg2
        // this generates the derivative of that
        self.visit_condition(condition, arg1, arg2)
    }
}

impl<'lt, 'mir: 'lt> RealBuiltInFunctionCall1pVisitor
    for ExpressionAutoDiff<'lt, 'mir, RealExpressionId>
{
    type T = Derivative;

    fn visit_sqrt(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;
        // f'/ ( 2*sqrt(f) )
        let two = self.gen_constant(2.0);
        let num = self.gen_binary_op(two, Multiply, self.current_expr);
        Some(self.gen_binary_op(inner, Divide, num))
    }

    fn visit_exp(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;
        // f'*exp(f)
        Some(self.gen_binary_op(inner, Multiply, self.current_expr))
    }

    fn visit_ln(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;
        // f'/f
        Some(self.gen_binary_op(inner, Divide, arg))
    }

    fn visit_log(&mut self, arg: RealExpressionId) -> Derivative {
        // (ln(f))' * log10_e
        let res = self.visit_ln(arg)?;
        let factor = self.gen_constant(std::f64::consts::LOG10_E);
        Some(self.gen_binary_op(factor, Multiply, res))
    }

    fn visit_abs(&mut self, arg: RealExpressionId) -> Derivative {
        // f < 0
        let zero = self.gen_constant(0.0);
        let condition = self.gen_lt_condition(arg, zero);

        let derivative = self.visit_real_expr(arg)?;
        // -f
        let negated = self.gen_neg(derivative);

        // abs(f) = if (f < 0) -f' else f'
        let expr = RealExpression::Condition(condition, negated, derivative);
        Some(self.add_to_mir(expr))
    }

    fn visit_floor(&mut self, _arg: RealExpressionId) -> Derivative {
        dispatch_late(
            Box::new(RoundingDerivativeNotFullyDefined {
                span: self.ad.mir[self.current_expr].span,
            }),
            self.current_expr.into(),
        );
        None
    }

    fn visit_ceil(&mut self, _arg: RealExpressionId) -> Derivative {
        dispatch_late(
            Box::new(RoundingDerivativeNotFullyDefined {
                span: self.ad.mir[self.current_expr].span,
            }),
            self.current_expr.into(),
        );
        None
    }

    fn visit_sin(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;
        let outer = self.gen_math_function(Cos, arg);
        Some(self.gen_binary_op(inner, Multiply, outer))
    }

    fn visit_cos(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;
        let sin = self.gen_math_function(Sin, arg);
        let outer = self.gen_neg(sin);
        Some(self.gen_binary_op(inner, Multiply, outer))
    }

    fn visit_tan(&mut self, arg: RealExpressionId) -> Derivative {
        // f'*(1+tan^2(f))
        let inner = self.visit_real_expr(arg)?;
        let squred = self.gen_binary_op(self.current_expr, Multiply, self.current_expr);
        let one = self.gen_constant(1.0);
        let sum = self.gen_binary_op(one, Sum, squred);
        Some(self.gen_binary_op(inner, Multiply, sum))
    }

    fn visit_arcsin(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;

        // 1 - f²
        let sqrt_arg = self.gen_one_plus_minus_squared(true, arg);

        // sqrt(1-f²)
        let den = self.gen_math_function(Sqrt, sqrt_arg);

        // f'/sqrt(1-f²)
        Some(self.gen_binary_op(inner, Divide, den))
    }

    fn visit_arccos(&mut self, arg: RealExpressionId) -> Derivative {
        // - (arcsin(f)')
        let darcsin = self.visit_arcsin(arg)?;
        Some(self.gen_neg(darcsin))
    }

    fn visit_arctan(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;

        // 1-f²
        let den = self.gen_one_plus_minus_squared(false, arg);

        // f'/(1-f²)
        Some(self.gen_binary_op(inner, Divide, den))
    }

    fn visit_sinh(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;
        let outer = self.gen_math_function(CosH, arg);
        Some(self.gen_binary_op(inner, Multiply, outer))
    }

    fn visit_cosh(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;
        let outer = self.gen_math_function(SinH, arg);
        Some(self.gen_binary_op(inner, Multiply, outer))
    }

    fn visit_tanh(&mut self, arg: RealExpressionId) -> Derivative {
        // f'*(1-(tanh(f))²)
        let inner = self.visit_real_expr(arg)?;
        // 1-(tanh(f))²
        let outer = self.gen_one_plus_minus_squared(true, self.current_expr);
        Some(self.gen_binary_op(inner, Multiply, outer))
    }

    fn visit_arcsinh(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;

        // 1 + f²
        let sqrt_arg = self.gen_one_plus_minus_squared(false, arg);

        // sqrt(1+f²)
        let den = self.gen_math_function(Sqrt, sqrt_arg);

        // f'/sqrt(1+f²)
        Some(self.gen_binary_op(inner, Divide, den))
    }

    fn visit_arccosh(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;

        // 1-f²
        let minus_sqrt_arg = self.gen_one_plus_minus_squared(true, arg);

        // f²-1
        let sqrt_arg = self.gen_neg(minus_sqrt_arg);

        // sqrt(f²-1)
        let den = self.gen_math_function(Sqrt, sqrt_arg);

        // f'/sqrt(f²-1)
        Some(self.gen_binary_op(inner, Divide, den))
    }

    fn visit_arctanh(&mut self, arg: RealExpressionId) -> Derivative {
        let inner = self.visit_real_expr(arg)?;

        // 1-f²
        let den = self.gen_one_plus_minus_squared(true, arg);

        // f'/(1-f²)
        Some(self.gen_binary_op(inner, Divide, den))
    }
}

impl<'lt, 'mir: 'lt> IntegerExprVisitor for ExpressionAutoDiff<'lt, 'mir, IntegerExpressionId> {
    type T = Derivative;

    #[inline]
    fn visit_integer_expr(&mut self, expr: IntegerExpressionId) -> Derivative {
        let old = self.current_expr;
        self.current_expr = expr;
        let res = walk_integer_expression(self, expr);
        self.current_expr = old;
        res
    }
    fn mir(&self) -> &Mir {
        self.ad.mir
    }

    fn visit_literal(&mut self, _val: i64) -> Derivative {
        None
    }

    fn visit_binary_operator(
        &mut self,
        lhs: IntegerExpressionId,
        op: Node<IntegerBinaryOperator>,
        rhs: IntegerExpressionId,
    ) -> Derivative {
        self.visit_integer_binary_op(lhs, op.contents, rhs)
    }

    fn visit_integer_comparison(
        &mut self,
        _lhs: IntegerExpressionId,
        _op: Node<ComparisonOperator>,
        _rhs: IntegerExpressionId,
    ) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::Comparison);
        None
    }

    fn visit_real_comparison(
        &mut self,
        _lhs: RealExpressionId,
        _op: Node<ComparisonOperator>,
        _rhs: RealExpressionId,
    ) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::Comparison);
        None
    }

    fn visit_unary_op(&mut self, op: Node<UnaryOperator>, arg: IntegerExpressionId) -> Derivative {
        match op.contents {
            UnaryOperator::ArithmeticNegate => {
                let arg = self.visit_integer_expr(arg)?;
                Some(self.gen_neg(arg))
            }
            UnaryOperator::ExplicitPositive => self.visit_integer_expr(arg),
            UnaryOperator::BitNegate => {
                self.undefined_derivative(UndefinedDerivative::BitWiseOp);
                None
            }
            UnaryOperator::LogicNegate => {
                self.undefined_derivative(UndefinedDerivative::LogicOp);
                None
            }
        }
    }

    fn visit_condition(
        &mut self,
        cond: IntegerExpressionId,
        true_expr: IntegerExpressionId,
        false_expr: IntegerExpressionId,
    ) -> Derivative {
        let true_expr = self.visit_integer_expr(true_expr);
        let false_expr = self.visit_integer_expr(false_expr);
        let (true_expr, false_expr) = self.convert_to_paired(true_expr, false_expr)?;
        let expr = RealExpression::Condition(cond, true_expr, false_expr);
        Some(self.add_to_mir(expr))
    }

    fn visit_min(&mut self, arg1: IntegerExpressionId, arg2: IntegerExpressionId) -> Derivative {
        // arg1 < arg2
        let condition = self.gen_lt_condition(arg1, arg2);
        // max = if (arg1 < arg2) arg1 else arg2
        // this generates the derivative of that
        self.visit_condition(condition, arg1, arg2)
    }

    fn visit_max(&mut self, arg1: IntegerExpressionId, arg2: IntegerExpressionId) -> Derivative {
        // arg2 < arg1
        let condition = self.gen_lt_condition(arg2, arg1);
        // max = if (arg2 < arg1) arg1 else arg2
        // this generates the derivative of that
        self.visit_condition(condition, arg1, arg2)
    }

    fn visit_abs(&mut self, arg: IntegerExpressionId) -> Derivative {
        // f < 0
        let zero = self.gen_int_constant(0);
        let condition = self.gen_lt_condition(arg, zero);

        let derivative = self.visit_integer_expr(arg)?;
        // -f
        let negated = self.gen_neg(derivative);

        // abs(f) = if (f < 0) -f' else f'
        let expr = RealExpression::Condition(condition, negated, derivative);
        Some(self.add_to_mir(expr))
    }

    fn visit_variable_reference(&mut self, var: VariableId) -> Derivative {
        let var = self.ad.mir.derivative_var(var, self.unknown);
        Some(self.add_to_mir(RealExpression::VariableReference(var)))
    }

    fn visit_parameter_reference(&mut self, param: ParameterId) -> Derivative {
        self.param_derivative(param)
    }

    fn visit_real_cast(&mut self, expr: RealExpressionId) -> Derivative {
        ExpressionAutoDiff {
            current_expr: expr,
            ad: self.ad,
            unknown: self.unknown,
        }
        .visit_real_expr(expr)
    }

    fn visit_port_connected(&mut self, _port: PortId) -> Derivative {
        None
    }

    fn visit_param_given(&mut self, _param: ParameterId) -> Derivative {
        None
    }

    fn visit_port_reference(&mut self, _port: PortId) -> Derivative {
        unimplemented!("Ditigal")
    }

    fn visit_net_reference(&mut self, _net: NetId) -> Derivative {
        unimplemented!("Ditigal")
    }

    fn visit_string_eq(
        &mut self,
        _lhs: StringExpressionId,
        _rhs: StringExpressionId,
    ) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::Comparison);
        None
    }

    fn visit_string_neq(
        &mut self,
        _lhs: StringExpressionId,
        _rhs: StringExpressionId,
    ) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::Comparison);
        None
    }
}

impl<'lt, 'mir: 'lt> IntegerBinaryOperatorVisitor
    for ExpressionAutoDiff<'lt, 'mir, IntegerExpressionId>
{
    type T = Derivative;

    fn visit_sum(&mut self, lhs: IntegerExpressionId, rhs: IntegerExpressionId) -> Derivative {
        let dlhs = self.visit_integer_expr(lhs);
        let drhs = self.visit_integer_expr(rhs);
        self.derivative_sum(dlhs, drhs)
    }

    fn visit_diff(&mut self, lhs: IntegerExpressionId, rhs: IntegerExpressionId) -> Derivative {
        let dlhs = self.visit_integer_expr(lhs);
        let drhs = self.visit_integer_expr(rhs);
        let drhs = drhs.map(|drhs| self.gen_neg(drhs));
        self.derivative_sum(dlhs, drhs)
    }

    fn visit_mul(&mut self, lhs: IntegerExpressionId, rhs: IntegerExpressionId) -> Derivative {
        let dlhs = self.visit_integer_expr(lhs);
        let drhs = self.visit_integer_expr(rhs);
        let lhs = self.add_to_mir(IntegerConversion(lhs));
        let rhs = self.add_to_mir(IntegerConversion(rhs));
        self.mul_derivative(lhs, dlhs, rhs, drhs)
    }

    fn visit_quotient(&mut self, lhs: IntegerExpressionId, rhs: IntegerExpressionId) -> Derivative {
        let dlhs = self.visit_integer_expr(lhs);
        let drhs = self.visit_integer_expr(rhs);
        let lhs = self.add_to_mir(IntegerConversion(lhs));
        let rhs = self.add_to_mir(IntegerConversion(rhs));
        self.quotient_derivative(lhs, dlhs, rhs, drhs)
    }

    fn visit_pow(&mut self, lhs: IntegerExpressionId, rhs: IntegerExpressionId) -> Derivative {
        let dlhs = self.visit_integer_expr(lhs);
        let drhs = self.visit_integer_expr(rhs);
        let lhs = self.add_to_mir(IntegerConversion(lhs));
        let rhs = self.add_to_mir(IntegerConversion(rhs));
        let original = self.add_to_mir(RealExpression::IntegerConversion(self.current_expr));
        self.pow_derivative(lhs, dlhs, rhs, drhs, original)
    }

    fn visit_mod(&mut self, _lhs: IntegerExpressionId, _rhs: IntegerExpressionId) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::Modulus);
        None
    }

    fn visit_shiftl(&mut self, lhs: IntegerExpressionId, rhs: IntegerExpressionId) -> Derivative {
        // ln(2)*lhs*rhs'
        let product = if let Some(drhs) = self.visit_integer_expr(rhs) {
            let lhs = self.add_to_mir(IntegerConversion(lhs));
            let ln2 = self.gen_constant(std::f64::consts::LN_2);
            let product = self.gen_binary_op(ln2, Multiply, lhs);
            Some(self.gen_binary_op(product, Multiply, drhs))
        } else {
            None
        };

        //lhs'
        let dlhs = self.visit_integer_expr(lhs);

        // lhs' + ln(2)*lhs*rhs'
        let sum = self.derivative_sum(dlhs, product)?;
        let expr = self.add_to_mir(IntegerConversion(self.current_expr));

        // (lhs' + ln(2)*lhs*rhs')* 2**rhs
        Some(self.gen_binary_op(sum, Multiply, expr))
    }

    fn visit_shiftr(&mut self, lhs: IntegerExpressionId, rhs: IntegerExpressionId) -> Derivative {
        // -ln(2)*lhs*rhs'
        let product = if let Some(drhs) = self.visit_integer_expr(rhs) {
            let lhs = self.add_to_mir(IntegerConversion(lhs));
            let ln2 = self.gen_constant(-1.0 * std::f64::consts::LN_2);
            let product = self.gen_binary_op(ln2, Multiply, lhs);
            Some(self.gen_binary_op(product, Multiply, drhs))
        } else {
            None
        };

        //lhs'
        let dlhs = self.visit_integer_expr(lhs);

        // lhs' + (- ln(2)*lhs*rhs')
        let sum = self.derivative_sum(dlhs, product)?;
        let expr = self.add_to_mir(IntegerConversion(self.current_expr));

        // (lhs' - ln(2)*lhs*rhs')* 2**-rhs
        Some(self.gen_binary_op(sum, Multiply, expr))
    }

    fn visit_xor(&mut self, _lhs: IntegerExpressionId, _rhs: IntegerExpressionId) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::BitWiseOp);
        None
    }

    fn visit_nxor(&mut self, _lhs: IntegerExpressionId, _rhs: IntegerExpressionId) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::BitWiseOp);
        None
    }

    fn visit_and(&mut self, _lhs: IntegerExpressionId, _rhs: IntegerExpressionId) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::BitWiseOp);
        None
    }

    fn visit_or(&mut self, _lhs: IntegerExpressionId, _rhs: IntegerExpressionId) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::BitWiseOp);
        None
    }

    fn visit_logic_and(
        &mut self,
        _lhs: IntegerExpressionId,
        _rhs: IntegerExpressionId,
    ) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::LogicOp);
        None
    }

    fn visit_logic_or(
        &mut self,
        _lhs: IntegerExpressionId,
        _rhs: IntegerExpressionId,
    ) -> Derivative {
        self.undefined_derivative(UndefinedDerivative::LogicOp);
        None
    }
}

impl<'lt> AutoDiff<'lt> {
    pub fn partial_derivative(
        &mut self,
        expr: ExpressionId,
        derive_by: Unknown,
    ) -> RealExpressionId {
        match expr {
            ExpressionId::Real(expr) => ExpressionAutoDiff {
                current_expr: expr,
                unknown: derive_by,
                ad: self,
            }
            .run(),
            ExpressionId::Integer(expr) => ExpressionAutoDiff {
                current_expr: expr,
                unknown: derive_by,
                ad: self,
            }
            .run(),
            ExpressionId::String(expr) => {
                self.errors
                    .add(OnlyNumericExpressionsCanBeDerived(self.mir[expr].span));

                // Just a placeholder
                self.mir
                    .real_expressions
                    .push(Node::new(RealExpression::Literal(0.0), DUMMY_SP))
            }
        }
    }
}
