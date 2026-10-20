# Separate state per environment. The state bucket must be created beforehand
# (see docs/runbook.md) and should differ between staging and production.
bucket       = "REPLACE_ME-ccpr-tfstate-production"
key          = "cloudflare/production.tfstate"
region       = "us-east-1"
encrypt      = true
use_lockfile = true
