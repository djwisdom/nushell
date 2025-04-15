use crate::engine::StateWorkingSet;

use super::{
    Block, Expr, Expression, ListItem, MatchPattern, Pattern, PipelineRedirection, RecordItem,
};

/// Result of find_map closure
#[derive(Default)]
pub enum FindMapResult<T> {
    Found(T),
    #[default]
    Continue,
    Stop,
}

/// Trait for traversing the AST
pub trait Traverse {
    /// Generic function that do flat_map on an AST node.
    /// Concatenates all recursive results on sub-expressions
    /// into the `results` accumulator.
    ///
    /// # Arguments
    /// * `f` - function that generates leaf elements
    /// * `results` - accumulator
    fn flat_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F, results: &mut Vec<T>)
    where
        F: Fn(&'a Expression) -> Vec<T>;

    /// Generic function that do find_map on an AST node.
    /// Return the first result found by applying `f` on sub-expressions.
    ///
    /// # Arguments
    /// * `f` - function that overrides the default behavior
    fn find_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F) -> Option<T>
    where
        F: Fn(&'a Expression) -> FindMapResult<T>;
}

impl Traverse for Block {
    fn flat_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F, results: &mut Vec<T>)
    where
        F: Fn(&'a Expression) -> Vec<T>,
    {
        for pipeline in self.pipelines.iter() {
            for element in pipeline.elements.iter() {
                element.expr.flat_map(working_set, f, results);
                if let Some(redir) = &element.redirection {
                    redir.flat_map(working_set, f, results);
                };
            }
        }
    }

    fn find_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F) -> Option<T>
    where
        F: Fn(&'a Expression) -> FindMapResult<T>,
    {
        self.pipelines.iter().find_map(|pipeline| {
            pipeline.elements.iter().find_map(|element| {
                element.expr.find_map(working_set, f).or(element
                    .redirection
                    .as_ref()
                    .and_then(|redir| redir.find_map(working_set, f)))
            })
        })
    }
}

impl Traverse for PipelineRedirection {
    fn flat_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F, results: &mut Vec<T>)
    where
        F: Fn(&'a Expression) -> Vec<T>,
    {
        let mut recur = |expr: &'a Expression| expr.flat_map(working_set, f, results);

        match self {
            PipelineRedirection::Single { target, .. } => target.expr().map(recur),
            PipelineRedirection::Separate { out, err } => {
                out.expr().map(&mut recur);
                err.expr().map(&mut recur)
            }
        };
    }

    fn find_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F) -> Option<T>
    where
        F: Fn(&'a Expression) -> FindMapResult<T>,
    {
        let recur = |expr: &'a Expression| expr.find_map(working_set, f);
        match self {
            PipelineRedirection::Single { target, .. } => target.expr().and_then(recur),
            PipelineRedirection::Separate { out, err } => {
                [out, err].iter().filter_map(|t| t.expr()).find_map(recur)
            }
        }
    }
}

impl Traverse for Expression {
    fn flat_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F, results: &mut Vec<T>)
    where
        F: Fn(&'a Expression) -> Vec<T>,
    {
        // leaf elements generated by `f` for this expression
        results.extend(f(self));
        let mut recur = |expr: &'a Expression| expr.flat_map(working_set, f, results);

        match &self.expr {
            Expr::RowCondition(block_id)
            | Expr::Subexpression(block_id)
            | Expr::Block(block_id)
            | Expr::Closure(block_id) => {
                let block = working_set.get_block(block_id.to_owned());
                block.flat_map(working_set, f, results)
            }
            Expr::Range(range) => {
                for sub_expr in [&range.from, &range.next, &range.to].into_iter().flatten() {
                    recur(sub_expr);
                }
            }
            Expr::Call(call) => {
                for arg in &call.arguments {
                    if let Some(sub_expr) = arg.expr() {
                        recur(sub_expr);
                    }
                }
            }
            Expr::ExternalCall(head, args) => {
                recur(head.as_ref());
                for arg in args {
                    recur(arg.expr());
                }
            }
            Expr::UnaryNot(expr) | Expr::Collect(_, expr) => recur(expr.as_ref()),
            Expr::BinaryOp(lhs, op, rhs) => {
                recur(lhs);
                recur(op);
                recur(rhs);
            }
            Expr::MatchBlock(matches) => {
                for (pattern, expr) in matches {
                    pattern.flat_map(working_set, f, results);
                    expr.flat_map(working_set, f, results);
                }
            }
            Expr::List(items) => {
                for item in items {
                    match item {
                        ListItem::Item(expr) | ListItem::Spread(_, expr) => recur(expr),
                    }
                }
            }
            Expr::Record(items) => {
                for item in items {
                    match item {
                        RecordItem::Spread(_, expr) => recur(expr),
                        RecordItem::Pair(key, val) => {
                            recur(key);
                            recur(val);
                        }
                    }
                }
            }
            Expr::Table(table) => {
                for column in &table.columns {
                    recur(column);
                }
                for row in &table.rows {
                    for item in row {
                        recur(item);
                    }
                }
            }
            Expr::ValueWithUnit(vu) => recur(&vu.expr),
            Expr::FullCellPath(fcp) => recur(&fcp.head),
            Expr::Keyword(kw) => recur(&kw.expr),
            Expr::StringInterpolation(vec) | Expr::GlobInterpolation(vec, _) => {
                for item in vec {
                    recur(item);
                }
            }
            Expr::AttributeBlock(ab) => {
                for attr in &ab.attributes {
                    recur(&attr.expr);
                }
                recur(&ab.item);
            }

            _ => (),
        };
    }

    fn find_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F) -> Option<T>
    where
        F: Fn(&'a Expression) -> FindMapResult<T>,
    {
        // behavior overridden by f
        match f(self) {
            FindMapResult::Found(t) => Some(t),
            FindMapResult::Stop => None,
            FindMapResult::Continue => {
                let recur = |expr: &'a Expression| expr.find_map(working_set, f);
                match &self.expr {
                    Expr::RowCondition(block_id)
                    | Expr::Subexpression(block_id)
                    | Expr::Block(block_id)
                    | Expr::Closure(block_id) => {
                        // Clone the block_id to create an owned value
                        let block_id = block_id.to_owned();
                        let block = working_set.get_block(block_id);
                        block.find_map(working_set, f)
                    }
                    Expr::Range(range) => [&range.from, &range.next, &range.to]
                        .iter()
                        .find_map(|e| e.as_ref().and_then(recur)),
                    Expr::Call(call) => call
                        .arguments
                        .iter()
                        .find_map(|arg| arg.expr().and_then(recur)),
                    Expr::ExternalCall(head, args) => {
                        recur(head.as_ref()).or(args.iter().find_map(|arg| recur(arg.expr())))
                    }
                    Expr::UnaryNot(expr) | Expr::Collect(_, expr) => recur(expr.as_ref()),
                    Expr::BinaryOp(lhs, op, rhs) => recur(lhs).or(recur(op)).or(recur(rhs)),
                    Expr::MatchBlock(matches) => matches.iter().find_map(|(pattern, expr)| {
                        pattern.find_map(working_set, f).or(recur(expr))
                    }),
                    Expr::List(items) => items.iter().find_map(|item| match item {
                        ListItem::Item(expr) | ListItem::Spread(_, expr) => recur(expr),
                    }),
                    Expr::Record(items) => items.iter().find_map(|item| match item {
                        RecordItem::Spread(_, expr) => recur(expr),
                        RecordItem::Pair(key, val) => [key, val].into_iter().find_map(recur),
                    }),
                    Expr::Table(table) => table
                        .columns
                        .iter()
                        .find_map(recur)
                        .or(table.rows.iter().find_map(|row| row.iter().find_map(recur))),
                    Expr::ValueWithUnit(vu) => recur(&vu.expr),
                    Expr::FullCellPath(fcp) => recur(&fcp.head),
                    Expr::Keyword(kw) => recur(&kw.expr),
                    Expr::StringInterpolation(vec) | Expr::GlobInterpolation(vec, _) => {
                        vec.iter().find_map(recur)
                    }
                    Expr::AttributeBlock(ab) => ab
                        .attributes
                        .iter()
                        .find_map(|attr| recur(&attr.expr))
                        .or_else(|| recur(&ab.item)),

                    _ => None,
                }
            }
        }
    }
}

impl Traverse for MatchPattern {
    fn flat_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F, results: &mut Vec<T>)
    where
        F: Fn(&'a Expression) -> Vec<T>,
    {
        let mut recur_pattern =
            |pattern: &'a MatchPattern| pattern.flat_map(working_set, f, results);

        match &self.pattern {
            Pattern::Expression(expr) => expr.flat_map(working_set, f, results),
            Pattern::List(patterns) | Pattern::Or(patterns) => {
                for pattern in patterns {
                    recur_pattern(pattern);
                }
            }
            Pattern::Record(entries) => {
                for (_, p) in entries {
                    recur_pattern(p);
                }
            }
            _ => (),
        };

        if let Some(g) = self.guard.as_ref() {
            g.flat_map(working_set, f, results);
        }
    }

    fn find_map<'a, T, F>(&'a self, working_set: &'a StateWorkingSet, f: &F) -> Option<T>
    where
        F: Fn(&'a Expression) -> FindMapResult<T>,
    {
        let recur = |expr: &'a Expression| expr.find_map(working_set, f);
        let recur_pattern = |pattern: &'a MatchPattern| pattern.find_map(working_set, f);
        match &self.pattern {
            Pattern::Expression(expr) => recur(expr),
            Pattern::List(patterns) | Pattern::Or(patterns) => {
                patterns.iter().find_map(recur_pattern)
            }
            Pattern::Record(entries) => entries.iter().find_map(|(_, p)| recur_pattern(p)),
            _ => None,
        }
        .or(self.guard.as_ref().and_then(|g| recur(g)))
    }
}
