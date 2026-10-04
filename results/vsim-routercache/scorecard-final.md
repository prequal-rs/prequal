
### burst

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| c400 (8) | 269ms / 1.1s (95%) | 302ms / 1.8s (95%) | llmd-optimized 302ms / 1.8s (95%) | win 0.58x |

### cache-two-routers

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| 4.00 (8) | 31ms / 32ms (68%) | 31ms / 32ms (50%) | llmd-optimized 31ms / 32ms (50%) | tie |
| 6.00 (8) | 31ms / 32ms (65%) | 31ms / 32ms (49%) | llmd-optimized 31ms / 32ms (49%) | tie |
| 8.00 (8) | 31ms / 32ms (63%) | 32ms / 33ms (50%) | llmd-optimized 32ms / 33ms (50%) | tie |
| 10.00 (8) | 32ms / 32ms (62%) | 32ms / 32ms (50%) | llmd-optimized 32ms / 32ms (50%) | tie |
| 12.00 (8) | 32ms / 33ms (61%) | 32ms / 38ms (48%) | llmd-optimized 32ms / 38ms (48%) | win 0.88x |
| 14.00 (8) | 32ms / 35ms (61%) | 32ms / 180ms (49%) | llmd-optimized 32ms / 180ms (49%) | win 0.20x |
| 16.00 (8) | 32ms / 72ms (59%) | 32ms / 429ms (49%) | llmd-optimized 32ms / 429ms (49%) | win 0.17x |

### cache

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| 4.00 (8) | 30ms / 32ms (69%) | 30ms / 32ms (69%) | llmd-optimized 30ms / 32ms (69%) | tie |
| 6.00 (8) | 30ms / 32ms (67%) | 30ms / 32ms (67%) | llmd-optimized 30ms / 32ms (67%) | tie |
| 8.00 (8) | 31ms / 32ms (67%) | 31ms / 32ms (68%) | llmd-optimized 31ms / 32ms (68%) | tie |
| 10.00 (8) | 31ms / 32ms (68%) | 31ms / 69ms (69%) | llmd-optimized 31ms / 69ms (69%) | win 0.46x |
| 12.00 (8) | 31ms / 32ms (66%) | 31ms / 171ms (68%) | llmd-optimized 31ms / 171ms (68%) | win 0.19x |
| 14.00 (8) | 32ms / 33ms (67%) | 32ms / 747ms (68%) | llmd-optimized 32ms / 747ms (68%) | win 0.04x |
| 16.00 (8) | 32ms / 92ms (66%) | 32ms / 1.1s (67%) | llmd-optimized 32ms / 1.1s (67%) | win 0.08x |

### canonical

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| 3.00 (8) | 55ms / 61ms (99%) | 55ms / 60ms (99%) | llmd-optimized 55ms / 60ms (99%) | tie |
| 10.00 (8) | 36ms / 39ms (100%) | 36ms / 39ms (100%) | llmd-optimized 36ms / 39ms (100%) | tie |
| 15.00 (8) | 42ms / 45ms (100%) | 42ms / 45ms (100%) | llmd-optimized 42ms / 45ms (100%) | tie |
| 20.00 (8) | 51ms / 63ms (100%) | 51ms / 63ms (100%) | llmd-optimized 51ms / 63ms (100%) | tie |
| 22.00 (8) | 2.3s / 4.8s (93%) | 2.3s / 4.8s (93%) | llmd-optimized 2.3s / 4.8s (93%) | tie |
| 25.00 (8) | 14.5s / 17.1s (87%) | 14.5s / 17.1s (87%) | llmd-optimized 14.5s / 17.1s (87%) | tie |
| 30.00 (8) | 31.0s / 34.3s (87%) | 31.1s / 34.3s (87%) | llmd-optimized 31.1s / 34.3s (87%) | tie |
| 35.00 (8) | 52.1s / 55.8s (87%) | 52.3s / 55.8s (87%) | llmd-optimized 52.3s / 55.8s (87%) | tie |
| 40.00 (8) | 96.5s / 102.5s (87%) | 96.8s / 102.6s (87%) | llmd-optimized 96.8s / 102.6s (87%) | tie |

### mixed

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| 3.00 (8) | 8.4s / 16.8s (92%) | 7.6s / 17.0s (92%) | llmd-optimized 7.6s / 17.0s (92%) | tie |
| 10.00 (8) | 112ms / 329ms (96%) | 95ms / 375ms (97%) | llmd-optimized 95ms / 375ms (97%) | win 0.88x |
| 15.00 (8) | 83ms / 108ms (99%) | 79ms / 94ms (100%) | llmd-optimized 79ms / 94ms (100%) | LOSS 1.14x |
| 20.00 (8) | 5.2s / 9.7s (93%) | 4.4s / 10.9s (94%) | llmd-optimized 4.4s / 10.9s (94%) | win 0.89x |
| 22.00 (8) | 19.8s / 24.8s (90%) | 20.0s / 27.0s (90%) | llmd-optimized 20.0s / 27.0s (90%) | tie |
| 25.00 (8) | 40.3s / 48.5s (90%) | 40.0s / 52.0s (90%) | llmd-optimized 40.0s / 52.0s (90%) | tie |
| 30.00 (8) | 64.6s / 73.1s (90%) | 65.6s / 76.1s (90%) | llmd-optimized 65.6s / 76.1s (90%) | tie |
| 35.00 (8) | 95.6s / 108.1s (90%) | 95.4s / 108.5s (90%) | llmd-optimized 95.4s / 108.5s (90%) | tie |
| 40.00 (8) | 157.4s / 181.7s (90%) | 154.9s / 179.9s (90%) | llmd-optimized 154.9s / 179.9s (90%) | tie |

### two-routers

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| 3.00 (8) | 220ms / 859ms (80%) | 4.0s / 7.7s (67%) | llmd-optimized 4.0s / 7.7s (67%) | win 0.11x |
| 10.00 (8) | 110ms / 338ms (90%) | 619ms / 1.3s (71%) | llmd-optimized 619ms / 1.3s (71%) | win 0.27x |
| 15.00 (8) | 96ms / 311ms (96%) | 494ms / 864ms (71%) | llmd-optimized 494ms / 864ms (71%) | win 0.36x |
| 20.00 (8) | 4.3s / 7.3s (84%) | 17.4s / 22.1s (68%) | llmd-optimized 17.4s / 22.1s (68%) | win 0.33x |
| 22.00 (8) | 19.0s / 23.3s (78%) | 40.2s / 46.0s (67%) | llmd-optimized 40.2s / 46.0s (67%) | win 0.51x |
| 25.00 (8) | 37.1s / 42.7s (77%) | 67.0s / 72.8s (67%) | llmd-optimized 67.0s / 72.8s (67%) | win 0.59x |
| 30.00 (8) | 59.8s / 66.2s (77%) | 98.1s / 105.5s (67%) | llmd-optimized 98.1s / 105.5s (67%) | win 0.63x |
| 35.00 (8) | 88.6s / 95.2s (77%) | 135.1s / 142.3s (67%) | llmd-optimized 135.1s / 142.3s (67%) | win 0.67x |
| 40.00 (8) | 145.0s / 158.9s (77%) | 207.4s / 222.8s (67%) | llmd-optimized 207.4s / 222.8s (67%) | win 0.71x |

### unique

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| 3.00 (8) | 121ms / 125ms (83%) | 120ms / 125ms (83%) | llmd-optimized 120ms / 125ms (83%) | tie |
| 10.00 (8) | 116ms / 182ms (83%) | 116ms / 181ms (83%) | llmd-optimized 116ms / 181ms (83%) | tie |
| 15.00 (8) | 156ms / 219ms (83%) | 156ms / 219ms (83%) | llmd-optimized 156ms / 219ms (83%) | tie |
| 20.00 (8) | 600ms / 1.9s (83%) | 601ms / 1.9s (83%) | llmd-optimized 601ms / 1.9s (83%) | tie |
| 22.00 (8) | 8.9s / 11.1s (83%) | 8.9s / 11.2s (83%) | llmd-optimized 8.9s / 11.2s (83%) | tie |
| 25.00 (8) | 20.6s / 23.3s (83%) | 20.6s / 23.3s (83%) | llmd-optimized 20.6s / 23.3s (83%) | tie |
| 30.00 (8) | 37.0s / 40.2s (83%) | 37.1s / 40.2s (83%) | llmd-optimized 37.1s / 40.2s (83%) | tie |
| 35.00 (8) | 57.9s / 61.8s (83%) | 58.1s / 61.8s (83%) | llmd-optimized 58.1s / 61.8s (83%) | tie |
| 40.00 (8) | 102.3s / 108.9s (83%) | 102.6s / 109.0s (83%) | llmd-optimized 102.6s / 109.0s (83%) | tie |

### zipf

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| 3.00 (8) | 112ms / 442ms (92%) | 144ms / 439ms (92%) | llmd-optimized 144ms / 439ms (92%) | tie |
| 10.00 (8) | 105ms / 481ms (93%) | 106ms / 482ms (94%) | llmd-optimized 106ms / 482ms (94%) | tie |
| 15.00 (8) | 109ms / 487ms (95%) | 108ms / 488ms (95%) | llmd-optimized 108ms / 488ms (95%) | tie |
| 20.00 (8) | 111ms / 490ms (96%) | 111ms / 487ms (96%) | llmd-optimized 111ms / 487ms (96%) | tie |
| 22.00 (8) | 95ms / 227ms (98%) | 117ms / 1.1s (97%) | llmd-optimized 117ms / 1.1s (97%) | win 0.21x |
| 25.00 (8) | 99ms / 282ms (98%) | 2.0s / 4.1s (97%) | llmd-optimized 2.0s / 4.1s (97%) | win 0.07x |
| 30.00 (8) | 2.3s / 7.8s (96%) | 8.8s / 14.6s (96%) | llmd-optimized 8.8s / 14.6s (96%) | win 0.54x |
| 35.00 (8) | 15.2s / 19.4s (95%) | 23.6s / 28.6s (96%) | llmd-optimized 23.6s / 28.6s (96%) | win 0.68x |
| 40.00 (8) | 35.8s / 43.6s (95%) | 47.3s / 58.4s (95%) | llmd-optimized 47.3s / 58.4s (95%) | win 0.75x |

**vs llmd-optimized: 24 wins, 1 losses (mean p99 TTFT over seeds, ±10% = tie)**
