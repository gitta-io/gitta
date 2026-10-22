| Run | Instance | vCPU / RAM | $/h VM + 200 GB disk | Benchmark wall-clock | Billed (apply to destroy) | Cost | Replayed / conflicts / evaluated / breaks | Live recall | Live false alarms | Gated false alarms |
|---|---|---|---|---|---|---|---|---|---|---|
| Google Cloud 8 | e2-standard-8, europe-north1-a | 8 / 32 GB | 0.2951 + 0.0512 | 12.4 h | 13.2 h | $4.58 | 733 / 179 / 43 / 1 | 1/1 | 12/42 | 1/42 |
| AWS 8 | m6i.2xlarge, eu-north-1b | 8 / 32 GB | 0.4080 + 0.0229 | 10.1 h | 11.0 h | $4.72 | 733 / 179 / 44 / 1 | 1/1 | 13/43 | 0/43 |
| Google Cloud 4 | e2-standard-4, europe-north1-a | 4 / 16 GB | 0.1476 + 0.0512 | 19.2 h | 19.3 h | $3.84 | 733 / 179 / 42 / 1 | 1/1 | 10/41 | 1/41 |
| AWS 4 | m6i.xlarge, eu-north-1b | 4 / 16 GB | 0.2040 + 0.0229 | 14.7 h | 14.8 h | $3.35 | 733 / 179 / 44 / 1 | 1/1 | 13/43 | 0/43 |
| Azure 4 | Standard_D4s_v5, swedencentral | 4 / 16 GB | 0.2040 + 0.0573 | 16.7 h | 17.6 h | $4.59 | 733 / 179 / 45 / 1 | 1/1 | 13/44 | 0/44 |
