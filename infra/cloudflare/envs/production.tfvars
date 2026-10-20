environment           = "production"
cloudflare_account_id = "REPLACE_ME"
zone_id               = "REPLACE_ME"
hostname              = "registry.example.org"
enable_bot_fight_mode = true
enable_managed_waf    = false
# Set to true to let the Cloudflare-hosted registry serve AWS S3 buckets.
enable_aws_sts_principal = false
# Staging shares the zone: production's state owns the zone-level rules for both hosts.
manage_zone_rules   = true
zone_rule_hostnames = ["registry.example.org", "registry-staging.example.org"]
