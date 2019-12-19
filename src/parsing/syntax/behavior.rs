//  * ******************************************************************************************
//  * Copyright (c) 2019 Pascal Kuthe. This file is part of the rust_adms project.
//  * It is subject to the license terms in the LICENSE file found in the top-level directory
//  *  of this distribution and at  https://gitlab.com/jamescoding/rust_adms/blob/master/LICENSE.
//  *  No part of rust_adms, including this file, may be copied, modified, propagated, or
//  *  distributed except according to the terms contained in the LICENSE file.
//  * *******************************************************************************************

use super::*;

impl ParseTreeToRawAstFolder {
    mk_fold_fn!(block (self,parse_tree_node,description) {
        self.state_stack.push(State::BLOCK);
        let ast_node;
        if description.peek().unwrap().as_rule() == Rule::IDENTIFIER {
            let block_name = identifier_string(description.next().unwrap());
            ast_node = self.ast.arena.new_node(ast::Node::BLOCK(Some(block_name)));
            while description.peek().unwrap().as_rule() != Rule::BEHAVIORAL_STMT {
                self.fold_item_with_attributes(description.next().unwrap(), ast_node)?
            }
        } else {
            ast_node = self.ast.arena.new_node(ast::Node::BLOCK(None))
        }
        for child in description{
            self.fold_item_with_attributes(child, ast_node)?;
        }
        self.state_stack.pop();
        Ok([ast_node])
    });

    mk_fold_fn!(condition(self,parse_tree_node,description){
        let ast_node = self.ast.arena.new_node(ast::Node::Cond);
        let expr = self.process_expression(description.next().unwrap())?;
        ast_node.append(expr, &mut self.ast.arena);
        self.fold_item_with_attributes(description.next().unwrap(), ast_node)?; //if body
        while let Some(next_stmt) = description.next() {
                match next_stmt.as_rule() {
                    Rule::EXPRESSION => {
                        let expr = self.process_expression(next_stmt)?;
                        ast_node.append(expr, &mut self.ast.arena);
                        self.fold_item_with_attributes(description.next().unwrap(), ast_node)?;
                    },
                    Rule::BEHAVIORAL_STMT => {
                        self.fold_item_with_attributes(next_stmt, ast_node)?;
                    },

                    _ => unexpected_rule!(parse_tree_node),
                }
        };
        Ok([ast_node])
    });

    mk_fold_fn!(contribute(self,parse_tree_node,description){
        let ast_node = self.ast.arena.new_node(ast::Node::Contribute);
        //TODO port probe function call
        let lvalue = self.process_function_call(description.next().unwrap(),false)?;
        let rvalue = self.process_expression(description.next().unwrap())?;
        ast_node.append(lvalue, &mut self.ast.arena);
        ast_node.append(rvalue, &mut self.ast.arena);
        Ok([ast_node])
    });

    mk_fold_fn!(variable_assignment(self,parse_tree_node,description){
        let lvalues = description.next().unwrap().into_inner();
        let value = self.process_expression(description.next().unwrap())?;
        let nodes = Vec::new();
        for ident in lvalues {
            //TODO Range
            let node = self.ast.arena.new_node(ast::Node::ASSIGN);
            let ident_node = self.ast.arena.new_node(ast::Node::Reference(hierarchical_identifier_string(ident)));
            node.append(ident_node, &mut self.ast.arena);
            node.append(value, &mut self.ast.arena);
        }
        Ok(nodes)
    });

    //TODO incoporate this into function call processing
//TODO do port probing seperatly
    /* fn process_branch_reference(&mut self, value: ParseTreeNode) -> Result<NodeId> {
     trace!("Processing branch reference from {:0?}",value);
     if !self.state_stack.contains(&State::AnalogBehavior) {
         return error("Branch acess is only allowed in analog behavior!", value.as_span());
     }
     let mut description = value.into_inner();
     let nature_node = description.next().unwrap().into_inner().next().unwrap();
     let nature =
         match nature_node.as_rule() {
             Rule::TOK_POTENTIAL => ast::node_types::NatureAccess::POTENTIAL,
             Rule::TOK_FLOW => ast::node_types::NatureAccess::FLOW,
             Rule::IDENTIFIER => ast::node_types::NatureAccess::UNRESOLVED(identifier_string(nature_node)),
             _ => unexpected_rule!(nature_node)
         };
     let port_branch = description.peek().unwrap().as_rule() == Rule::PORT_BRANCH_IDENTIFIER;
     let ast_node = self.ast.arena.new_node(ast::Node::BranchRef(nature, port_branch));
     description = description.next().unwrap().into_inner();
     while let Some(reference_name) = description.next() {
         let reference_node = self.process_hierarchical_id(reference_name)?;
         ast_node.append(reference_node, &mut self.ast.arena);
         if_rule!(let Some(range) = description.next() where Rule::SINGEL_RANGE => {
             self.process_single_range(range, reference_node)?
         });
     }
     Ok(ast_node)
 }*/
    pub(super) fn process_function_call(&mut self, value: ParseTreeNode, system_function: bool) -> SyntaxResult<NodeId> {
        trace!("Processing function call from {}", value);
        let mut description = value.into_inner();
        let hierarchical_identifier = if system_function {
            vec!(identifier_string(description.next().unwrap()))
        } else if description.peek().unwrap().as_rule() == Rule::STANDARD_FUNCTIONS {
            vec!(as_string!(description.next().unwrap()))
        } else {
            hierarchical_identifier_string(description.next().unwrap())
        };
        let node = self.ast.arena.new_node(ast::Node::Fcall { hierarchical_identifier: hierarchical_identifier.clone(), system_function });
        for arg in description {
            let arg_node = self.process_expression(arg)?;
            node.append(arg_node, &mut self.ast.arena);
        }
        debug!("Processed {}function {:?} call with {} arguments", if system_function { "System" } else { "" }, hierarchical_identifier, node.children(&self.ast.arena).count());
        Ok(node)
    }
}
