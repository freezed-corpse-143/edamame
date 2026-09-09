# Display Math Smoke Test

This document exercises the `$$...$$` display-math pipeline (RaTeX →
text-SVG → shared fontdb → image). Open it in `edamame` and scroll
through each block to verify rendering, theme-aware glyph color, cell
sizing, caching, and the in-place reveal (move the cursor into a formula
in Rendered mode). Inline `$...$` math renders as source-equivalent text
in phase 1 — the checks near the end pin that.

## Basics

A lone display formula on its own paragraph promotes to an image block:

$$
x^2 + y^2 = z^2
$$

Mass–energy equivalence:

$$
E = mc^2
$$

## Fractions and roots

$$
\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}
$$

$$
\frac{1}{1 + \frac{1}{1 + \frac{1}{1 + x}}}
$$

## Sums, products, integrals

$$
\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}
$$

$$
\int_{-\infty}^{\infty} e^{-x^2} \, dx = \sqrt{\pi}
$$

$$
\prod_{k=1}^{n} k = n!
$$

## Greek, sub/superscripts, accents

$$
\alpha + \beta_i^2 - \gamma^{n+1} + \hat{x} + \bar{y} + \vec{v}
$$

## Matrices

$$
A = \begin{bmatrix} 1 & 2 \\ 3 & 4 \end{bmatrix} 
$$

## Stacked blocks (no blank line between)

The two formulas below are separated only by their delimiters — no blank
line. pulldown-cmark folds them into one paragraph; the promotion pass
must re-split them into two independent image blocks:

$$
X = \begin{bmatrix} 1 & 2 \end{bmatrix}
$$
$$
Y = \begin{bmatrix} 3 & 4 & 5 \end{bmatrix}
$$

## Delimiters and stretchy braces

$$
f(x) = \left( \frac{x^2 + 1}{x - 1} \right)^2
$$

$$
\left\{ x \in \mathbb{R} : x > 0 \right\}
$$

## Inline math stays text (phase 1)

The Pythagorean relation $a^2 + b^2 = c^2$ appears inline here and must
render as its source, not promote to an image — inline beautification is
out of phase-1 scope.

## Literal dollars must not become math

A price like $5 and another of $10 (a single, unclosed `$` each) must
stay literal text — not be swallowed as an inline-math span.

An escaped \$100 with a backslash must also stay literal.

## Below

Trailing text after the last formula, to confirm the block boundary and
the reserved-row accounting return cleanly to prose.
