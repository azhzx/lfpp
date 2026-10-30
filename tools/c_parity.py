#!/usr/bin/env python3
"""LFPP 与 C 预处理器 (gcc -E -P) 的差分对比探针。

每个用例给出一段等价的 C 源码和 LFPP 源码，分别预处理后按“去掉所有空白”
比较输出。用例带有期望标签：

    match        期望与 C 一致
    diff         已知/有意的行为差异

报告分为四类：
    OK-MATCH      期望一致且一致
    REGRESSION    期望一致但不一致（需要关注）
    KNOWN-DIFF    期望有差异且有差异（记录在案）
    FIXED         期望有差异但现在一致了（说明行为已改变）

用法:
    python3 tools/c_parity.py [--verbose] [--lfpp PATH] [--cc PATH]

默认在以下位置查找 lfpp: ./target/debug/lfpp, ../target/debug/lfpp,
target/release/lfpp, 以及 PATH。也支持环境变量 LFPP。
"""

import argparse
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def find_lfpp():
    env = os.environ.get("LFPP")
    if env:
        return env
    cands = [
        os.path.join(ROOT, "target", "debug", "lfpp"),
        os.path.join(ROOT, "target", "release", "lfpp"),
        os.path.join(ROOT, os.pardir, "target", "debug", "lfpp"),
        os.path.join(ROOT, os.pardir, "target", "release", "lfpp"),
    ]
    for c in cands:
        if os.path.exists(c):
            return os.path.abspath(c)
    return shutil.which("lfpp")


CASES = []
def case(name, expect, c, l):
    CASES.append({"name": name, "expect": expect, "c": c, "l": l})


# ---------- 宏展开：应当一致 ----------
case("obj_simple", "match", "#define A 1\nA\n", "(%object_macro A 1%)\nA\n")
case("obj_self", "match", "#define A A\nA\n", "(%object_macro A A%)\nA\n")
case("obj_mutual", "match", "#define A B\n#define B A\nA\n",
     "(%object_macro A B%)\n(%object_macro B A%)\nA\n")
case("obj_chain", "match", "#define A B\n#define B C\n#define C 1\nA\n",
     "(%object_macro A B%)\n(%object_macro B C%)\n(%object_macro C 1%)\nA\n")
case("obj_redef_same", "match", "#define A 1\n#define A 1\nA\n",
     "(%object_macro A 1%)\n(%object_macro A 1%)\nA\n")
case("undef", "match", "#define A 1\n#undef A\nA\n",
     "(%object_macro A 1%)\n(%undefine A%)\nA\n")
case("undef_missing", "match", "#undef ZZZ\nZZZ\n", "(%undefine ZZZ%)\nZZZ\n")
case("undef_redefine", "match", "#define A 1\n#undef A\n#define A 2\nA\n",
     "(%object_macro A 1%)\n(%undefine A%)\n(%object_macro A 2%)\nA\n")
case("func_basic", "match", "#define id(x) x\nid(42)\n",
     "(%function_macro id(x) x%)\nid(42)\n")
case("func_name_only", "match", "#define f(x) x\nf\n", "(%function_macro f(x) x%)\nf\n")
case("func_space_paren", "match", "#define f(x) x\nf (1)\n",
     "(%function_macro f(x) x%)\nf (1)\n")
case("func_two_empty", "match", "#define f(a,b) a-b\nf(,)\n",
     "(%function_macro f(a,b) a-b%)\nf(,)\n")
case("func_nested_paren", "match", "#define f(x) x\nf((1,2))\n",
     "(%function_macro f(x) x%)\nf((1,2))\n")
case("arg_prescan", "match", "#define f() 2\n#define id(x) x\nid(f())\n",
     "(%function_macro f() 2%)\n(%function_macro id(x) x%)\nid(f())\n")
case("arg_prescan_rec", "match", "#define f(x) x+x\nf(f(1))\n",
     "(%function_macro f(x) x+x%)\nf(f(1))\n")
case("nested_call", "match",
     "#define add(a,b) a+b\n#define one 1\nadd(one, add(2,3))\n",
     "(%function_macro add(a,b) a+b%)\n(%object_macro one 1%)\nadd(one, add(2,3))\n")
case("no_parens_multi", "match", "#define f(x) x\nf(1)(2)\n",
     "(%function_macro f(x) x%)\nf(1)(2)\n")
case("defer_obj_func", "match", "#define f(x) x\n#define g f\ng(1)\n",
     "(%function_macro f(x) x%)\n(%object_macro g f%)\ng(1)\n")
case("unavail_fof", "match", "#define f(a) a\nf(f)(1)\n",
     "(%function_macro f(a) a%)\nf(f)(1)\n")
case("foo_bar", "match", "#define foo(x) bar x\n#define bar(x) foo x\nfoo(2)(1)\n",
     "(%function_macro foo(x) bar x%)\n(%function_macro bar(x) foo x%)\nfoo(2)(1)\n")
case("empty_body", "match", "#define E\nE x\n", "(%object_macro E%)\nE x\n")
case("macro_in_string", "match", "#define A 1\n\"A\" A\n",
     "(%object_macro A 1%)\n\"A\" A\n")
case("macro_arg_string", "match", "#define f(x) x\nf(\"A\")\n",
     "(%function_macro f(x) x%)\nf(\"A\")\n")
case("comment_paren", "match", "#define f(x) x\nf/**/(1)\n",
     "(%function_macro f(x) x%)\nf/**/(1)\n")
case("comment_arg", "match", "#define f(a,b) a-b\nf(1/**/,2)\n",
     "(%function_macro f(a,b) a-b%)\nf(1/**/,2)\n")
case("undef_in_skipped_branch", "match",
     "#define A 1\n#if 0\n#undef A\n#endif\nA\n",
     "(%object_macro A 1%)\n(%if 0%)\n(%undefine A%)\n(%endif%)\nA\n")
case("error_in_skipped_branch", "match", "#if 0\n#error nope\n#endif\nOK\n",
     "(%if 0%)\n(%error \"nope\"%)\n(%endif%)\nOK\n")
case("if_nested", "match",
     "#if 1\n#if 0\nA\n#else\nB\n#endif\nC\n#endif\n",
     "(%if 1%)\n(%if 0%)\nA\n(%else%)\nB\n(%endif%)\nC\n(%endif%)\n")

# ---------- 变参：应当一致 ----------
case("variadic_basic", "match", "#define M(a,...) a __VA_ARGS__\nM(1,2,3)\n",
     "(%function_macro M(a, ...) a (%va_args%)%)\nM(1,2,3)\n")
case("variadic_only", "match", "#define M(...) __VA_ARGS__\nM(1,2)\n",
     "(%function_macro M(...) (%va_args%)%)\nM(1,2)\n")
case("variadic_empty", "match", "#define M(a,...) a __VA_ARGS__\nM(1)\n",
     "(%function_macro M(a, ...) a (%va_args%)%)\nM(1)\n")
case("variadic_trailing_empty", "match",
     "#define M(a, ...) [a] __VA_OPT__(,) __VA_ARGS__\nM(1)\nM(1,)\nM(1,2)\n",
     "(%function_macro M(a, ...) [a](%va_opt(,)%)(%va_args%)%)\nM(1)\nM(1,)\nM(1,2)\n")
case("vaopt", "match",
     "#define M(a,...) a __VA_OPT__(,) __VA_ARGS__\nM(1)\nM(1,2)\n",
     "(%function_macro M(a, ...) a (%va_opt(,)%)(%va_args%)%)\nM(1)\nM(1,2)\n")
case("counter", "match", "__COUNTER__ __COUNTER__\n", "(%get_counter%) (%get_counter%)\n")

# ---------- %if 表达式：应当一致 ----------
case("cond_defined", "match",
     "#define FOO 1\n#if defined(FOO)\n1\n#else\n0\n#endif\n",
     "(%object_macro FOO 1%)\n(%if (%is_defined FOO%)%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_div", "match", "#if 7/2 == 3\n1\n#else\n0\n#endif\n",
     "(%if 7/2 == 3%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_mod", "match", "#if 7%3 == 1\n1\n#else\n0\n#endif\n",
     "(%if 7%3 == 1%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_mod_neg", "match", "#if -7%3 == -1\n1\n#else\n0\n#endif\n",
     "(%if -7%3 == -1%)\n1\n(%else%)\n0\n(%endif%)\n")
case("if_macro_value", "match",
     "#define N 3\n#if N*2==6\n1\n#else\n0\n#endif\n",
     "(%object_macro N 3%)\n(%if N*2==6%)\n1\n(%else%)\n0\n(%endif%)\n")
case("if_paren_all", "match", "#if (2+3)*2 == 10\n1\n#else\n0\n#endif\n",
     "(%if (2+3)*2 == 10%)\n1\n(%else%)\n0\n(%endif%)\n")
case("not_macro", "match", "#define Z 0\n#if !Z\n1\n#else\n0\n#endif\n",
     "(%object_macro Z 0%)\n(%if !Z%)\n1\n(%else%)\n0\n(%endif%)\n")
case("empty_in_if", "match", "#define E\n#if 0 E == 0\n1\n#else\n0\n#endif\n",
     "(%object_macro E%)\n(%if 0 E == 0%)\n1\n(%else%)\n0\n(%endif%)\n")

# ---------- 自赋值的 token paste（顶层）：应当一致 ----------
case("concat_compound", "match", "#define CAT(a,b) a##b\nCAT(+,=)\n",
     "+(%concat%)= \n")

# ---------- 阴影/hide-set 硬用例（去掉换行后）：应当一致 ----------
case("gccdoc_singleline", "match", r"""#define x    3
#define f(a) f(x * (a))
#undef  x
#define x    2
#define g    f
#define z    z[0]
#define h    g(!
#define m(a) a(w)
#define w    0,1
f(y+1) + f(f(z)) % t(t(g)(0) + t)(1);
g(x+(3,4)-w) | h 5) & m(f)^m(m);
""", r"""(%object_macro x 3%)
(%function_macro f(a) f(x * (a))%)
(%undefine x%)
(%object_macro x 2%)
(%object_macro g f%)
(%object_macro z z[0]%)
(%object_macro h g(!%)
(%function_macro m(a) a(w)%)
(%object_macro w 0,1%)
f(y+1) + f(f(z)) % t(t(g)(0) + t)(1);
g(x+(3,4)-w) | h 5) & m(f)^m(m);
""")

# ---------- C 的 # / ## 宏体内语义 ----------
case("paste_chain", "match", "#define CAT3(a,b,c) a##b##c\nCAT3(x,y,z)\n",
     "(%function_macro CAT3(a,b,c) a (%concat%) b (%concat%) c%)\nCAT3(x,y,z)\n")
case("paste_empty_left", "match", "#define CAT(a,b) a##b\nCAT(,x)\n",
     "(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(,x)\n")
case("paste_empty_right", "match", "#define CAT(a,b) a##b\nCAT(x,)\n",
     "(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(x,)\n")
case("paste_both_empty", "match", "#define CAT(a,b) a##b\nCAT(,)\n",
     "(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(,)\n")
case("paste_object", "match", "#define GLUE foo##bar\nGLUE\n",
     "(%object_macro GLUE foo (%concat%) bar%)\nGLUE\n")
case("paste_rescan", "match", "#define CAT(a,b) a##b\n#define xy 9\nCAT(x,y)\n",
     "(%function_macro CAT(a,b) a (%concat%) b%)\n(%object_macro xy 9%)\nCAT(x,y)\n")
case("paste_integer", "match", "#define CAT(a,b) a##b\nCAT(1,2)\n",
     "(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(1,2)\n")
case("paste_variadic_empty", "match", "#define CAT(a,...) a##__VA_ARGS__\nCAT(x,)\n",
     "(%function_macro CAT(a,...) a (%concat%) (%va_args%)%)\nCAT(x,)\n")
case("stringize_adjacent", "match", "#define STR(x) #x\nSTR(a+b)\n",
     "(%function_macro STR(x) (%to_string%) x%)\nSTR(a+b)\n")
case("stringize_spaced", "match", "#define STR(x) #x\nSTR(a + b)\n",
     "(%function_macro STR(x) (%to_string%) x%)\nSTR(a + b)\n")
case("stringize_string_tokens", "match", '#define STR(x) #x\nSTR(a "b" c)\n',
     '(%function_macro STR(x) (%to_string%) x%)\nSTR(a "b" c)\n')
case("stringize_empty_arg", "match", "#define STR(x) #x\nSTR()\n",
     "(%function_macro STR(x) (%to_string%) x%)\nSTR()\n")
case("stringize_va", "match", "#define STR(...) #__VA_ARGS__\nSTR(a,b)\n",
     "(%function_macro STR(...) (%to_string%) (%va_args%)%)\nSTR(a,b)\n")
case("stringize_va_spaced", "match", "#define STR(...) #__VA_ARGS__\nSTR(a, b)\n",
     "(%function_macro STR(...) (%to_string%) (%va_args%)%)\nSTR(a, b)\n")

# ---------- 更多回归用例 ----------
case("obj_redef_diff", "match", "#define A 1\n#define A 2\nA\n",
     "(%object_macro A 1%)\n(%object_macro A 2%)\nA\n")
case("func_redef", "match", "#define f(x) x\n#define f(x) x+x\nf(1)\n",
     "(%function_macro f(x) x%)\n(%function_macro f(x) x+x%)\nf(1)\n")
case("func_newline_paren", "match", "#define f(x) x\nf\n(1)\n",
     "(%function_macro f(x) x%)\nf\n(1)\n")
case("func_empty_arg", "match", "#define f(x) [x]\nf()\n",
     "(%function_macro f(x) [x]%)\nf()\n")
case("dup_param", "match", "#define f(a,a) a\nf(1,2)\n",
     "(%function_macro f(a,a) a%)\nf(1,2)\n")
case("concat_in_macro", "match", "#define CAT(a,b) a##b\nCAT(foo,bar)\n",
     "(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(foo,bar)\n")
case("stringize_in_macro", "match", "#define STR(x) #x\nSTR(hello world)\n",
     "(%function_macro STR(x) (%to_string%) x%)\nSTR(hello world)\n")
case("to_string_no_quotes", "match", "#define S(x) #x\nS(\"hi\")\n",
     "(%to_string%) \"hi\"\n")
case("stringize_multi", "match", "#define S(x) #x\nS(a b c)\n",
     "(%function_macro S(x) (%to_string%) x%)\nS(a b c)\n")
case("concat_expands_operand", "match", "#define CAT(a,b) a##b\n#define A foo\nCAT(A,bar)\n",
     "(%function_macro CAT(a,b) a (%concat%) b%)\n(%object_macro A foo%)\nCAT(A,bar)\n")
case("cond_undef_ident", "match", "#if FOO\n1\n#else\n0\n#endif\n",
     "(%if FOO%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_char", "match", "#if 'A' == 65\n1\n#else\n0\n#endif\n",
     "(%if 'A' == 65%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_shift", "match", "#if (1<<3)==8\n1\n#else\n0\n#endif\n",
     "(%if (1<<3)==8%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_andor_prec", "match", "#if 1 || 0 && 0\n1\n#else\n0\n#endif\n",
     "(%if 1 || 0 && 0%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_chain_cmp", "match", "#if 3 > 2 > 1\n1\n#else\n0\n#endif\n",
     "(%if 3 > 2 > 1%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_ternary", "match", "#if 0 ? 0 : 1\n1\n#else\n0\n#endif\n",
     "(%if 0 ? 0 : 1%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_not_prec", "match", "#if !0 + 1\n1\n#else\n0\n#endif\n",
     "(%if !0 + 1%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_octal", "match", "#if 010 == 8\n1\n#else\n0\n#endif\n",
     "(%if 010 == 8%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_trailing_tokens", "match", "#if 1 2\n1\n#else\n0\n#endif\n",
     "(%if 1 2%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_bitand", "match", "#if 1 & 0\n1\n#else\n0\n#endif\n",
     "(%if 1 & 0%)\n1\n(%else%)\n0\n(%endif%)\n")
case("cond_bignum", "match",
     "#if 0xFFFFFFFFFFFFFFFF == 18446744073709551615ULL\n1\n#else\n0\n#endif\n",
     "(%if 0xFFFFFFFFFFFFFFFF == 18446744073709551615%)\n1\n(%else%)\n0\n(%endif%)\n")
case("overflow_dec", "match",
     "#if 99999999999999999999 > 0\n1\n#else\n0\n#endif\n",
     "(%if 99999999999999999999 > 0%)\n1\n(%else%)\n0\n(%endif%)\n")
case("signed_unsigned", "match", "#if -1 < 1u\n1\n#else\n0\n#endif\n",
     "(%if -1 < 1u%)\n1\n(%else%)\n0\n(%endif%)\n")
case("if_comma", "match", "#if (1,2)==2\n1\n#else\n0\n#endif\n",
     "(%if (1,2)==2%)\n1\n(%else%)\n0\n(%endif%)\n")
case("shortcircuit_and", "match", "#if 0 && 1/0\n1\n#else\n0\n#endif\n",
     "(%if 0 && 1/0%)\n1\n(%else%)\n0\n(%endif%)\n")
case("shortcircuit_or", "match", "#if 1 || 1/0\n1\n#else\n0\n#endif\n",
     "(%if 1 || 1/0%)\n1\n(%else%)\n0\n(%endif%)\n")
case("line_splice", "match", "#define A 1 \\\n + 2\nA\n",
     "(%object_macro A 1 \\\n + 2%)\nA\n")
case("bitnot", "match", "#if ~0 == -1\n1\n#else\n0\n#endif\n",
     "(%if ~0 == -1%)\n1\n(%else%)\n0\n(%endif%)\n")
case("comment_rendered", "match", "#define AB 1\nA/**/B\n",
     "(%object_macro AB 1%)\nA/**/B\n")


def normalize(s):
    # Comments are whitespace to the preprocessor; LFPP preserves them in its
    # output while gcc replaces them with a space.
    s = re.sub(r"/\*.*?\*/", " ", s, flags=re.S)
    s = re.sub(r"//[^\n]*", " ", s)
    return re.sub(r"\s+", "", s)


def run(cmd, src, path, env=None):
    with open(path, "w") as f:
        f.write(src)
    r = subprocess.run(cmd + [path], capture_output=True, text=True, env=env)
    return r.returncode, r.stdout, r.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--lfpp", default=find_lfpp())
    ap.add_argument("--cc", default=os.environ.get("CC", "gcc"))
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    if not args.lfpp or not os.path.exists(args.lfpp):
        print("找不到 lfpp 可执行文件，请用 --lfpp 指定", file=sys.stderr)
        return 2

    import tempfile
    tmp = tempfile.mkdtemp(prefix="lfpp_parity_")
    cwd = os.path.join(tmp, "c.c")
    lwd = os.path.join(tmp, "l.pp")
    env = dict(os.environ)
    env["CARGO"] = "1"
    env["NO_COLOR"] = "1"

    buckets = {"OK-MATCH": [], "REGRESSION": [], "KNOWN-DIFF": [], "FIXED": []}
    details = {}
    for cs in CASES:
        crc, cout, cerr = run([args.cc, "-E", "-P", "-std=gnu23"], cs["c"], cwd)
        lrc, lout, lerr = run([args.lfpp], cs["l"], lwd, env)
        both_ok = crc == 0 and lrc == 0
        both_err = crc != 0 and lrc != 0
        # If both reject the input, the partial stdout gcc emits before
        # erroring is not meaningful; only the accept/reject decision matters.
        same = (both_ok and normalize(cout) == normalize(lout)) or both_err
        if cs["expect"] == "match":
            key = "OK-MATCH" if same else "REGRESSION"
        else:
            key = "KNOWN-DIFF" if not same else "FIXED"
        buckets[key].append(cs["name"])
        details[cs["name"]] = (cout, lout, cerr, lerr)

    for key in ["OK-MATCH", "KNOWN-DIFF", "FIXED", "REGRESSION"]:
        names = buckets[key]
        print(f"== {key} ({len(names)}) ==")
        for n in names:
            print(f"   {n}")
    if args.verbose:
        for n, (cout, lout, cerr, lerr) in details.items():
            print(f"\n### {n}\n  C : {cout!r}\n  L : {lout!r}")
            if cerr.strip():
                print(f"  Ce: {cerr.strip().splitlines()[0]}")
            if lerr.strip():
                print(f"  Le: {lerr.strip().splitlines()[0]}")
    print()
    print(f"总计 {len(CASES)} 用例；"
          f"regression={len(buckets['REGRESSION'])}，"
          f"known-diff={len(buckets['KNOWN-DIFF'])}，"
          f"fixed={len(buckets['FIXED'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
