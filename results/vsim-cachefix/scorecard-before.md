
### burst

| stage | prequal p90 / p99 (hits) | llmd-optimized | best other | vs llmd-optimized p99 |
|---|---|---|---|---|
| c400 (8) | 269ms / 1.1s (95%) | 302ms / 1.8s (95%) | llmd-optimized 302ms / 1.8s (95%) | win 0.58x |

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
| 3.00 (8) | 565ms / 2.0s (81%) | 4.5s / 8.4s (68%) | llmd-optimized 4.5s / 8.4s (68%) | win 0.24x |
| 10.00 (8) | 112ms / 367ms (90%) | 618ms / 1.4s (71%) | llmd-optimized 618ms / 1.4s (71%) | win 0.26x |
| 15.00 (8) | 121ms / 336ms (93%) | 497ms / 785ms (71%) | llmd-optimized 497ms / 785ms (71%) | win 0.43x |
| 20.00 (8) | 7.1s / 11.4s (84%) | 17.3s / 22.7s (68%) | llmd-optimized 17.3s / 22.7s (68%) | win 0.50x |
| 22.00 (8) | 22.3s / 27.8s (79%) | 40.7s / 47.4s (68%) | llmd-optimized 40.7s / 47.4s (68%) | win 0.59x |
| 25.00 (8) | 41.5s / 50.3s (78%) | 67.9s / 74.8s (68%) | llmd-optimized 67.9s / 74.8s (68%) | win 0.67x |
| 30.00 (8) | 66.3s / 73.5s (78%) | 99.7s / 110.0s (68%) | llmd-optimized 99.7s / 110.0s (68%) | win 0.67x |
| 35.00 (8) | 93.5s / 106.4s (78%) | 136.4s / 147.6s (68%) | llmd-optimized 136.4s / 147.6s (68%) | win 0.72x |
| 40.00 (8) | 148.4s / 168.4s (78%) | 208.9s / 227.3s (68%) | llmd-optimized 208.9s / 227.3s (68%) | win 0.74x |

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

**vs llmd-optimized: 17 wins, 1 losses (mean p99 TTFT over seeds, ±10% = tie)**
