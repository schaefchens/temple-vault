#!/usr/bin/env python3
"""How large may a hidden space be? Noise simulation for the Temple vault format (docs/format.md).

Sizes are counted in zones of ZONE_MB. The adversary opens the everyday space,
knows the open-source noise rules, and sees which parts of the file the
everyday space explains; everything else is unexplained.

LR = P(file | hidden space) / P(file | none): how much the file raises the odds
that a hidden space exists. inf means proof.

Pure Python, no dependencies: python3 tools/noise_sim.py
"""

import bisect
import math
import random

ZONE_MB = 16
MAX_ZONES = 1024  # 16 GB of support


def zones(mb):
    return round(mb / ZONE_MB)


def normalize(p):
    s = sum(p)
    return [x / s for x in p]


def delta(k=0):
    p = [0.0] * MAX_ZONES
    p[k] = 1.0
    return p


def convolve(p, q):
    r = [0.0] * MAX_ZONES
    nz = [(j, y) for j, y in enumerate(q) if y > 1e-30]
    for i, x in enumerate(p):
        if x <= 1e-30:
            continue
        for j, y in nz:
            if i + j >= MAX_ZONES:
                break
            r[i + j] += x * y
    return r


# --- Noise shapes ---------------------------------------------------------

def geometric(mean_mb, cap_mb=None):
    """Discrete exponential: P(k) = (1-q) q^k, optionally cut off at cap_mb."""
    m = mean_mb / ZONE_MB
    q = m / (1 + m)
    n = MAX_ZONES if cap_mb is None else zones(cap_mb) + 1
    return normalize([(1 - q) * q**k if k < n else 0.0 for k in range(MAX_ZONES)])


def uniform(lo_mb, hi_mb):
    lo, hi = zones(lo_mb), zones(hi_mb)
    return normalize([1.0 if lo <= k <= hi else 0.0 for k in range(MAX_ZONES)])


def peaked(mean_mb, shape):
    """Sum of `shape` geometrics with the same total mean (like many small steps)."""
    g = geometric(mean_mb / shape)
    p = g
    for _ in range(shape - 1):
        p = convolve(p, g)
    return normalize(p)


def lognormal(mean_mb, sigma):
    mu = math.log(mean_mb) - sigma**2 / 2

    def sf(x):
        if x <= 0:
            return 1.0
        return 0.5 * math.erfc((math.log(x) - mu) / (sigma * math.sqrt(2)))

    return normalize([sf((k - 0.5) * ZONE_MB) - sf((k + 0.5) * ZONE_MB)
                      for k in range(MAX_ZONES)])


# --- Statistics -------------------------------------------------------------

def quantile(p, f):
    c = 0.0
    for k, x in enumerate(p):
        c += x
        if c >= f:
            return k
    return MAX_ZONES - 1


def storage(p):
    mean = sum(k * x for k, x in enumerate(p)) * ZONE_MB
    return mean, quantile(p, 0.5) * ZONE_MB, quantile(p, 0.9) * ZONE_MB, quantile(p, 0.99) * ZONE_MB


def summary(rows):
    """rows: (lr, weight). Returns median LR, 90th percentile LR, share of proof."""
    rows = sorted(rows)
    total = sum(w for _, w in rows)

    def q(f):
        c = 0.0
        for lr, w in rows:
            c += w
            if c >= f * total:
                return lr
        return rows[-1][0]

    proof = sum(w for lr, w in rows if lr == math.inf) / total
    return q(0.5), q(0.9), proof


def fmt(lr):
    return "proof" if lr == math.inf else f"{lr:.2f}"


# --- 1. One gap: the hidden space sits next to the noise --------------------

def single_gap(p, h_mb):
    """Exact LR distribution for hidden spaces of h_mb added to noise p."""
    h = zones(h_mb)
    rows = []
    for r, w in enumerate(p):
        if w <= 0:
            continue
        u = r + h
        null = p[u] if u < MAX_ZONES else 0.0
        rows.append((w / null if null > 0 else math.inf, w))
    return summary(rows)


def shapes():
    mean = 384
    families = [
        ("uniform 128-640 MB", uniform(128, 640)),
        ("peaked (sum of 4 steps)", peaked(mean, 4)),
        ("exponential", geometric(mean)),
        ("exponential, cut at 2 GB", geometric(mean, 2048)),
        ("heavy tail (lognormal)", lognormal(mean, 1.5)),
    ]
    sizes = [16, 64, 128, 256, 384, 512]
    print(f"1. Noise shapes, mean {mean} MB. Storage in MB; then, per hidden size h,\n"
          "   LR median / 90th percentile / share of proof\n")
    print(f"{'shape':26} {'median':>6} {'p90':>5} {'p99':>5} | "
          + " | ".join(f"{'h=' + str(h):^17}" for h in sizes))
    for name, p in families:
        st = storage(p)
        cells = []
        for h in sizes:
            med, p90, proof = single_gap(p, h)
            cells.append(f"{fmt(med):>5} {fmt(p90):>5} {proof:5.0%}")
        print(f"{name:26} {st[1]:6.0f} {st[2]:5.0f} {st[3]:5.0f} | " + " | ".join(cells))
    print()


def sizing():
    print("2. Exponential noise: largest hidden space (MB) for a given LR\n")
    print(f"{'mean noise':>10} | {'LR 1.5':>6} {'LR 2':>6} {'LR 3':>6} | {'median':>6} {'p90':>6} {'p99':>6}")
    for mean in (256, 384, 512, 768):
        m = mean / ZONE_MB
        hs = [math.log(target) / math.log(1 + 1 / m) * ZONE_MB for target in (1.5, 2, 3)]
        st = storage(geometric(mean))
        print(f"{mean:>10} | " + " ".join(f"{h:6.0f}" for h in hs)
              + f" | {st[1]:6.0f} {st[2]:6.0f} {st[3]:6.0f}")
    print()
    print("3. Cutting the tail: share of hidden spaces that become provable (mean 384 MB)\n")
    print(f"{'cut at':>7} | " + " ".join(f"h={h:<5}" for h in (64, 256, 384)))
    for cap in (1024, 1536, 2048, 3072):
        p = geometric(384, cap)
        print(f"{cap:>4} MB | " + " ".join(f"{single_gap(p, h)[2]:7.2%}" for h in (64, 256, 384)))
    print()


# --- 2. Layout: gaps between the everyday space's blocks ---------------------

def sampler(p):
    cdf, c = [], 0.0
    for x in p:
        c += x
        cdf.append(c)
    return lambda rng: min(bisect.bisect_left(cdf, rng.random() * c), MAX_ZONES - 1)


_cache = {}


def conv_all(ps):
    """Convolution of long-lived pmfs; the cache keeps them alive so ids stay unique."""
    key = tuple(id(p) for p in ps)
    if key not in _cache:
        r = delta()
        for p in ps:
            r = convolve(r, p)
        _cache[key] = (list(ps), r)
    return _cache[key][1]


def layout(growth, reserve, step, on_create, lazy, h_mb, later, known=False, n=20000, seed=7):
    """LR for a hidden space created in one step after the everyday space (E).

    growth:    months at which E grows; E is created at month 0.
    step:      noise appended with each step (None: none).
    on_create: creation steps get noise too, not only growth steps.
    lazy:      the reserve is appended right after the first space's first
               block instead of at setup, before any space.
    later:     the hidden space appears at a uniform time over 12 months
               (otherwise on day one, before E grows).
    known:     the adversary knows when the hidden space was created.
    """
    times = [0.0] + list(growth) + [12.0]
    m = len(times) - 1                      # E's blocks; gap k follows block k
    comps = [[] for _ in range(m + 1)]
    comps[1 if lazy else 0].append(reserve)
    if step is not None:
        for k in range(1, m + 1):
            if k > 1 or on_create:
                comps[k].append(step)
    nulls = [conv_all(c) for c in comps]
    foot = delta(zones(h_mb))
    if step is not None and on_create:
        foot = convolve(foot, step)
    alts = [convolve(p, foot) for p in nulls]
    if later:
        pi = [0.0] + [(times[k] - times[k - 1]) / 12.0 for k in range(1, m + 1)]
    else:
        pi = [0.0, 1.0] + [0.0] * (m - 1)

    draws = [[sampler(p) for p in c] for c in comps]
    draw_foot = sampler(foot)
    pick = sampler(pi + [0.0] * (MAX_ZONES - len(pi)))
    rng = random.Random(seed)
    rows = []
    for _ in range(n):
        gaps = [sum(d(rng) for d in ds) for ds in draws]
        k = pick(rng)
        gaps[k] += draw_foot(rng)
        prior = [1.0 if j == k else 0.0 for j in range(m + 1)] if known else pi
        if any(g >= MAX_ZONES or nulls[j][g] == 0 for j, g in enumerate(gaps)):
            rows.append((math.inf, 1.0))
        else:
            rows.append((sum(prior[j] * alts[j][g] / nulls[j][g]
                             for j, g in enumerate(gaps) if prior[j] > 0), 1.0))
    total_noise = conv_all([c for cs in comps for c in cs])
    return summary(rows), storage(total_noise)


def layouts():
    reserve = geometric(384, 2048)
    s64, s128 = geometric(64), geometric(128)
    users = [("text only", []), ("audio, grows 4x", [1, 3, 6, 9])]
    designs = [
        ("reserve at setup, no step noise", False, None, False),
        ("reserve after 1st block, no step noise", True, None, False),
        ("  + 128 MB noise on every step", True, s128, True),
        ("  + 64 MB noise on growth steps", True, s64, False),
        ("  + 128 MB noise on growth steps", True, s128, False),
    ]
    cols = [(16, False, False), (256, False, False), (16, True, False), (256, True, False),
            (16, True, True), (256, True, True)]
    print("4. Layout: the adversary checks each gap between the everyday space's blocks.")
    print("   Reserve: exponential, mean 384 MB, cut at 2 GB. Hidden space created in one step:")
    print("   on day 1 (before the everyday space grows) or later (any time in 12 months; time")
    print("   unknown or known to the adversary). LR median / 90th percentile / share of proof\n")
    heads = ["day 1", "day 1", "later", "later", "later, known", "later, known"]
    print(f"{'everyday space':16} {'design':40} {'noise':>5} {'p90':>5} | "
          + " | ".join(f"{str(h) + ' MB ' + t:>19}" for (h, _, _), t in zip(cols, heads)))
    for uname, growth in users:
        for dname, lazy, st, on_create in designs:
            cells, store = [], None
            for h, later, known in cols:
                (med, p90, proof), store = layout(growth, reserve, st, on_create, lazy, h, later, known)
                cells.append(f"{fmt(med):>5} {fmt(p90):>5} {proof:6.1%}")
            print(f"{uname:16} {dname:40} {store[0]:5.0f} {store[2]:5.0f} | "
                  + " | ".join(f"{c:>19}" for c in cells))
    print()


if __name__ == "__main__":
    shapes()
    sizing()
    layouts()
