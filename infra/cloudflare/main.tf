locals {
  name = "ccpr-${var.environment}"
}

# --- D1 -----------------------------------------------------------------------

resource "cloudflare_d1_database" "registry" {
  account_id            = var.cloudflare_account_id
  name                  = "${local.name}-registry"
  primary_location_hint = var.d1_location_hint

  lifecycle {
    prevent_destroy = true
  }
}

# --- DNS ----------------------------------------------------------------------
# Proxied placeholder record; the Worker is attached with a route in wrangler.toml
# ("<hostname>/*"), so requests never reach the 100:: origin.

resource "cloudflare_dns_record" "registry" {
  zone_id = var.zone_id
  name    = var.hostname
  type    = "AAAA"
  content = "100::"
  proxied = true
  ttl     = 1
  comment = "CARLA Content Pack Registry (${var.environment}) - served by Workers"
}

# --- WAF: custom rules ------------------------------------------------------------

resource "cloudflare_ruleset" "firewall_custom" {
  zone_id     = var.zone_id
  name        = "${local.name} custom firewall"
  description = "Edge rules for the content pack registry"
  kind        = "zone"
  phase       = "http_request_firewall_custom"

  rules = [
    {
      description = "Development-only endpoints must never be reachable"
      expression  = "(http.host eq \"${var.hostname}\" and starts_with(http.request.uri.path, \"/login/mock\"))"
      action      = "block"
      enabled     = true
    },
    {
      description = "Only the methods the API uses"
      expression  = "(http.host eq \"${var.hostname}\" and not http.request.method in {\"GET\" \"HEAD\" \"POST\" \"PUT\" \"PATCH\" \"DELETE\" \"OPTIONS\"})"
      action      = "block"
      enabled     = true
    },
  ]
}

# --- WAF: rate limiting (per client IP, per colo) ------------------------------------

resource "cloudflare_ruleset" "ratelimit" {
  zone_id     = var.zone_id
  name        = "${local.name} rate limit"
  description = "Per-IP rate limit in front of the registry"
  kind        = "zone"
  phase       = "http_ratelimit"

  rules = [
    {
      description = "Per-IP request rate"
      expression  = "(http.host eq \"${var.hostname}\")"
      action      = "block"
      enabled     = true
      ratelimit = {
        characteristics     = ["cf.colo.id", "ip.src"]
        period              = var.ratelimit_period_seconds
        requests_per_period = var.ratelimit_requests_per_period
        mitigation_timeout  = var.ratelimit_mitigation_seconds
      }
    },
  ]
}

# --- WAF: managed rules (Pro+) --------------------------------------------------------

resource "cloudflare_ruleset" "managed_waf" {
  count       = var.enable_managed_waf ? 1 : 0
  zone_id     = var.zone_id
  name        = "${local.name} managed WAF"
  description = "Cloudflare Managed Ruleset"
  kind        = "zone"
  phase       = "http_request_firewall_managed"

  rules = [
    {
      description = "Cloudflare Managed Ruleset"
      expression  = "(http.host eq \"${var.hostname}\")"
      action      = "execute"
      enabled     = true
      action_parameters = {
        id = "efb7b8c949ac4650a09736fc376e9aee"
      }
    },
  ]
}

# --- Bots -----------------------------------------------------------------------

resource "cloudflare_bot_management" "zone" {
  count      = var.enable_bot_fight_mode ? 1 : 0
  zone_id    = var.zone_id
  fight_mode = true
}

# --- AWS principal for sts:AssumeRole (optional) ----------------------------------------
# No access key is created here, so no secret ever lands in Terraform state.
# Create one out-of-band and store it with `wrangler secret put` (see docs/runbook.md).

resource "aws_iam_user" "sts" {
  count = var.enable_aws_sts_principal ? 1 : 0
  name  = "${local.name}-cf-registry-sts"
  tags  = { app = "carla-content-pack-registry", environment = var.environment }
}

data "aws_iam_policy_document" "sts" {
  count = var.enable_aws_sts_principal ? 1 : 0
  statement {
    sid       = "AssumeUserOwnedReadRoles"
    actions   = ["sts:AssumeRole"]
    resources = [var.assumable_role_arn_pattern]
  }
}

resource "aws_iam_user_policy" "sts" {
  count  = var.enable_aws_sts_principal ? 1 : 0
  name   = "assume-user-roles"
  user   = aws_iam_user.sts[0].name
  policy = data.aws_iam_policy_document.sts[0].json
}
