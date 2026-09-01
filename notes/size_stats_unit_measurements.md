# PathMap<()> node measurements: before and after size optimizations

Measured 2026-09-01 with `cargo run --release --features counters --example size_stats`.

- Before: exact `master` revision `2192ebc`.
- After: `unit-size` revision `a317d47`, including `e67404c`.
- Value type: `()`; pointer mode: `slim_ptrs`.
- Machine-readable source: [size_stats_unit_measurements.json](size_stats_unit_measurements.json).

## What the byte columns mean

The report measures node-owned allocation bytes. `useful` is physical storage for live
representation fields—pointers, key bytes, masks, lengths/capacities, flags, and live
payload. `unused` is padding, spare capacity, unoccupied slots, and other allocated bytes
not carrying live representation. Therefore `useful + unused = requested` for every row,
and useful occupancy cannot exceed 100%.

Inline leaves are special: their 8-byte representation is inside a parent pointer word.
The report shows that embedded split separately as an attribution view, but does not add it
to heap totals because the parent word is already included in the parent node allocation.

The old logical `key_bytes / requested_bytes` value has deliberately been removed from the
main tables. Logical key-byte counts remain in JSON as diagnostics only; they are not a
physical occupancy measure.

## Dataset totals

| dataset | nodes | node allocations | owned requested bytes | useful bytes / occupancy | unused bytes / occupancy | inline embedded after |
|---|---:|---:|---:|---:|---:|---:|
| cities | 50,882 → 50,882 | 59,762 → 29,651 | 4,215,296 → 1,927,680 | 1,476,245 → 824,429 / 35.0% → 42.8% | 2,739,051 → 1,103,251 / 65.0% → 57.2% | 184,120 B (98,422 useful + 85,698 unused) |
| shakespeare_words | 28,787 → 28,787 | 35,536 → 19,977 | 2,409,104 → 1,326,072 | 903,688 → 429,765 / 37.5% → 32.4% | 1,505,416 → 896,307 / 62.5% → 67.6% | 76,360 B (30,908 useful + 45,452 unused) |
| shakespeare_sentences | 205,523 → 205,523 | 236,231 → 182,771 | 16,113,680 → 9,990,712 | 7,462,334 → 5,802,200 / 46.3% → 58.1% | 8,651,346 → 4,188,512 / 53.7% → 41.9% | 217,224 B (143,183 useful + 74,041 unused) |
| oeis | 519,978 → 519,978 | 531,089 → 513,786 | 35,411,904 → 31,679,784 | 26,918,754 → 27,020,954 / 76.0% → 85.3% | 8,493,150 → 4,658,830 / 24.0% → 14.7% | 138,424 B (74,266 useful + 64,158 unused) |
| big_logic_paths | 134,486 → 134,486 | 149,753 → 124,275 | 9,578,528 → 7,046,256 | 4,917,098 → 4,003,414 / 51.3% → 56.8% | 4,661,430 → 3,042,842 / 48.7% → 43.2% | 81,784 B (51,182 useful + 30,602 unused) |
| seq_u64 | 3,925 → 3,925 | 7,849 → 3,942 | 24,345,280 → 408,608 | 9,270,766 → 392,411 / 38.1% → 96.0% | 15,074,514 → 16,197 / 61.9% → 4.0% | — |
| rand_u64 | 1,038,518 → 1,038,518 | 1,104,874 → 159,682 | 89,903,744 → 19,706,488 | 24,159,073 → 12,884,201 / 26.9% → 65.4% | 65,744,671 → 6,822,287 / 73.1% → 34.6% | 7,553,000 B (5,663,038 useful + 1,889,962 unused) |
| rand_16b | 556,553 → 556,553 | 621,202 → 597,969 | 47,728,352 → 22,248,992 | 18,341,176 → 18,478,093 / 38.4% → 83.1% | 29,387,176 → 3,770,899 / 61.6% → 16.9% | — |
| rand_32b | 345,716 → 345,716 | 393,927 → 351,738 | 28,217,696 → 17,115,024 | 15,005,988 → 13,091,632 / 53.2% → 76.5% | 13,211,708 → 4,023,392 / 46.8% → 23.5% | — |
| rand_128b | 355,893 → 355,893 | 369,001 → 355,904 | 25,092,720 → 21,718,792 | 18,341,526 → 17,717,973 / 73.1% → 81.6% | 6,751,194 → 4,000,819 / 26.9% → 18.4% | 2,504 B (628 useful + 1,876 unused) |

`requested`, `useful`, and `unused` above are structural bytes before allocator rounding.
The counting allocator’s live-byte delta differs by at most a few bytes; those raw values
and glibc/jemalloc estimates are retained in the JSON.

## Per-node-type detail

Each table uses `before → after` pairs. A dash means that node type does not occur in that
revision/dataset. Inline leaves have zero node-owned allocation bytes by design.

### cities

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 42,002 → 15,810 | 42,002 → 15,810 | 2,688,128 → 1,011,840 | 525,485 → 257,793 | 2,162,643 → 754,047 | 19.6% → 25.5% |
| dense | 8,880 → — | 17,760 → — | 1,527,168 → — | 950,760 → — | 576,408 → — | 62.3% → — |
| inline | — → 23,015 | — → 0 | — → 0 | — → 0 | — → 0 | — → — |
| compact | — → 3,177 | — → 3,177 | — → 71,872 | — → 57,359 | — → 14,513 | — → 79.8% |
| sparse | — → 7,093 | — → 7,093 | — → 453,952 | — → 213,244 | — → 240,708 | — → 47.0% |
| unit | — → 1,787 | — → 3,571 | — → 390,016 | — → 296,033 | — → 93,983 | — → 75.9% |

### shakespeare_words

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 22,038 → 12,425 | 22,038 → 12,425 | 1,410,432 → 795,200 | 248,710 → 169,051 | 1,161,722 → 626,149 | 17.6% → 21.3% |
| dense | 6,749 → — | 13,498 → — | 998,672 → — | 654,978 → — | 343,694 → — | 65.6% → — |
| inline | — → 9,545 | — → 0 | — → 0 | — → 0 | — → 0 | — → — |
| compact | — → 68 | — → 68 | — → 1,320 | — → 1,094 | — → 226 | — → 82.9% |
| sparse | — → 6,005 | — → 6,005 | — → 384,320 | — → 144,688 | — → 239,632 | — → 37.6% |
| unit | — → 744 | — → 1,479 | — → 145,232 | — → 114,932 | — → 30,300 | — → 79.1% |

### shakespeare_sentences

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 174,815 → 84,242 | 174,815 → 84,242 | 11,188,160 → 5,391,488 | 4,297,367 → 2,384,150 | 6,890,793 → 3,007,338 | 38.4% → 44.2% |
| dense | 30,708 → — | 61,416 → — | 4,925,520 → — | 3,164,967 → — | 1,760,553 → — | 64.3% → — |
| inline | — → 27,153 | — → 0 | — → 0 | — → 0 | — → 0 | — → — |
| compact | — → 63,420 | — → 63,420 | — → 1,962,688 | — → 1,671,614 | — → 291,074 | — → 85.2% |
| sparse | — → 26,301 | — → 26,301 | — → 1,683,264 | — → 965,952 | — → 717,312 | — → 57.4% |
| unit | — → 4,407 | — → 8,808 | — → 953,272 | — → 780,484 | — → 172,788 | — → 81.9% |

### oeis

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 508,867 → 408,871 | 508,867 → 408,871 | 32,567,488 → 26,167,744 | 25,252,140 → 22,269,942 | 7,315,348 → 3,897,802 | 77.5% → 85.1% |
| dense | 11,111 → — | 22,222 → — | 2,844,416 → — | 1,666,614 → — | 1,177,802 → — | 58.6% → — |
| inline | — → 17,303 | — → 0 | — → 0 | — → 0 | — → 0 | — → — |
| compact | — → 82,693 | — → 82,693 | — → 3,201,008 | — → 2,839,952 | — → 361,056 | — → 88.7% |
| unit | — → 11,111 | — → 22,222 | — → 2,311,032 | — → 1,911,060 | — → 399,972 | — → 82.7% |

### big_logic_paths

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 119,219 → 81,053 | 119,219 → 81,053 | 7,630,016 → 5,187,392 | 3,528,812 → 2,674,982 | 4,101,204 → 2,512,410 | 46.2% → 51.6% |
| dense | 15,267 → — | 30,534 → — | 1,948,512 → — | 1,388,286 → — | 560,226 → — | 71.2% → — |
| inline | — → 10,223 | — → 0 | — → 0 | — → 0 | — → 0 | — → — |
| compact | — → 27,943 | — → 27,943 | — → 880,664 | — → 753,808 | — → 126,856 | — → 85.6% |
| sparse | — → 15,255 | — → 15,255 | — → 976,320 | — → 572,846 | — → 403,474 | — → 58.7% |
| unit | — → 12 | — → 24 | — → 1,880 | — → 1,778 | — → 102 | — → 94.6% |

### seq_u64

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 1 → 1 | 1 → 1 | 64 → 64 | 19 → 19 | 45 → 45 | 29.7% → 29.7% |
| dense | 3,924 → — | 7,848 → — | 24,345,216 → — | 9,270,747 → — | 15,074,469 → — | 38.1% → — |
| unit | — → 3,924 | — → 3,941 | — → 408,544 | — → 392,392 | — → 16,152 | — → 96.0% |

### rand_u64

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 972,162 → 28,037 | 972,162 → 28,037 | 62,218,368 → 1,794,368 | 10,831,996 → 448,333 | 51,386,372 → 1,346,035 | 17.4% → 25.0% |
| dense | 66,356 → — | 132,712 → — | 27,685,376 → — | 13,327,077 → — | 14,358,299 → — | 48.1% → — |
| inline | — → 944,125 | — → 0 | — → 0 | — → 0 | — → 0 | — → — |
| sparse | — → 1,067 | — → 1,067 | — → 68,288 | — → 37,204 | — → 31,084 | — → 54.5% |
| unit | — → 65,289 | — → 130,578 | — → 17,843,832 | — → 12,398,664 | — → 5,445,168 | — → 69.5% |

### rand_16b

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 491,904 → 8,170 | 491,904 → 8,170 | 31,481,856 → 522,880 | 9,453,691 → 262,701 | 22,028,165 → 260,179 | 30.0% → 50.2% |
| dense | 64,649 → — | 129,298 → — | 16,246,496 → — | 8,887,485 → — | 7,359,011 → — | 54.7% → — |
| compact | — → 483,734 | — → 483,734 | — → 11,609,616 | — → 9,674,724 | — → 1,934,892 | — → 83.3% |
| sparse | — → 23,233 | — → 23,233 | — → 1,486,912 | — → 1,204,156 | — → 282,756 | — → 81.0% |
| unit | — → 41,416 | — → 82,832 | — → 8,629,584 | — → 7,336,512 | — → 1,293,072 | — → 85.0% |

### rand_32b

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 297,505 → 59,790 | 297,505 → 59,790 | 19,040,320 → 3,826,560 | 9,540,570 → 1,873,075 | 9,499,750 → 1,953,485 | 50.1% → 49.0% |
| dense | 48,211 → — | 96,422 → — | 9,177,376 → — | 5,465,418 → — | 3,711,958 → — | 59.5% → — |
| compact | — → 237,715 | — → 237,715 | — → 9,021,080 | — → 7,905,210 | — → 1,115,870 | — → 87.6% |
| sparse | — → 42,189 | — → 42,189 | — → 2,700,096 | — → 1,889,259 | — → 810,837 | — → 70.0% |
| unit | — → 6,022 | — → 12,044 | — → 1,567,288 | — → 1,424,088 | — → 143,200 | — → 90.9% |

### rand_128b

| node kind | nodes | allocations | requested bytes | useful bytes | unused bytes | useful occupancy |
|---|---:|---:|---:|---:|---:|---:|
| list | 342,785 → 242,785 | 342,785 → 242,785 | 21,938,240 → 15,538,240 | 16,689,012 → 12,545,602 | 5,249,228 → 2,992,638 | 76.1% → 80.7% |
| dense | 13,108 → — | 26,216 → — | 3,154,480 → — | 1,652,514 → — | 1,501,966 → — | 52.4% → — |
| inline | — → 313 | — → 0 | — → 0 | — → 0 | — → 0 | — → — |
| compact | — → 99,687 | — → 99,687 | — → 4,866,160 | — → 4,239,028 | — → 627,132 | — → 87.1% |
| sparse | — → 12,784 | — → 12,784 | — → 818,176 | — → 487,639 | — → 330,537 | — → 59.6% |
| unit | — → 324 | — → 648 | — → 496,216 | — → 445,704 | — → 50,512 | — → 89.8% |

## Accounting formulas

The exact field-level formulas and layout assumptions are in the JSON `accounting_model`
section. In brief:

- Baseline `LineListNode`: useful = 6 B fixed metadata + live key bytes + 8 B per ordinary child word; an inline leaf word contributes only its key length plus one control byte.
- Baseline `DenseByteNode`: useful = 60 B header fields + 9 B per occupied entry; the remaining 7 B in each 16 B entry is padding.
- `InlineLeafNode`: 8 B embedded parent word, split as `key_len + 1` useful and `7 - key_len` unused; this split is attributed to the parent allocation.
- `CompactLeafNode`: useful header fields plus live key bytes; alignment, header pad, and spare key capacity are unused.
- `SparseByteNode`: useful fixed metadata plus live key bytes and live child words; unoccupied fixed arrays are unused.
- `UnitByteNode`: useful masks/header fields plus live child words; header tail padding and spare vector capacity are unused.
