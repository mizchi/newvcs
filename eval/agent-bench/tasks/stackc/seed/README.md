# stackc

Arithmetic expressions, a stack machine, a compiler from one to the other, and two rewritings
of expressions, with what is proved of them. Lean 4, nothing but its core library.

## Proved

Each theorem, ticked once its proof has no `sorry`:

- [ ] `run_append`: running two programs one after the other is running them joined
- [ ] `compile_run`: a compiled expression leaves its value on the stack
- [ ] `compile_correct`: the compiler is correct
- [ ] `eval_fold`: constant folding keeps the value
- [ ] `fold_size`: constant folding never grows an expression
- [ ] `eval_simplify`: simplification keeps the value
- [ ] `simplify_size`: simplification never grows an expression
- [ ] `compile_length`: a program is as long as its expression is large
- [ ] `compile_fold_correct`: compiling the folded expression is correct
- [ ] `compile_simplify_length`: simplifying first never makes a longer program

## Building

`lake build`
