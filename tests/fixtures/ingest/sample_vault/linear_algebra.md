---
tags: [math, linear-algebra]
---

# Linear Algebra

A study of [[vectors]] and [[matrices|matrix operations]]. The page is also
tagged #algebra inline.

## Vectors

Vectors have magnitude and direction. See [[dot_product]] for one common
operation.

### Dot Product

The dot product of two vectors is the sum of pairwise products:

```python
# `#not_a_tag` should be ignored because it's inside a fenced code block
def dot(a, b):
    return sum(x * y for x, y in zip(a, b))
```

> [!note] Tip
> Make sure both vectors have the same dimension before computing the dot product.

## Matrices

Matrices are 2D arrays of numbers. They compose linear maps between vector
spaces.
