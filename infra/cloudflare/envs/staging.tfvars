environment           = "staging"
cloudflare_account_id = "REPLACE_ME"
zone_id               = "REPLACE_ME"
hostname              = "registry-staging.example.org"
enable_bot_fight_mode = true
enable_managed_waf    = false
# Set to true to let the Cloudflare-hosted registry serve AWS S3 buckets.
enable_aws_sts_principal = false
# Zone-level WAF/rate-limit/bot rules are owned by the production state (same zone).
# Set to true if staging uses a separate zone.
manage_zone_rules = false
