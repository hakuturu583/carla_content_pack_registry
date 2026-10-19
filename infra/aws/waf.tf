# AWS WAF attached directly to the API Gateway REST API stage (no CloudFront).
resource "aws_wafv2_web_acl" "registry" {
  name  = "${local.name}-registry"
  scope = "REGIONAL"

  default_action {
    allow {}
  }

  rule {
    name     = "block-mock-login"
    priority = 0
    action {
      block {}
    }
    statement {
      byte_match_statement {
        search_string         = "/login/mock"
        positional_constraint = "STARTS_WITH"
        field_to_match {
          uri_path {}
        }
        text_transformation {
          priority = 0
          type     = "NONE"
        }
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "block-mock-login"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "rate-limit-per-ip"
    priority = 1
    action {
      block {}
    }
    statement {
      rate_based_statement {
        limit              = var.waf_rate_limit_per_5min
        aggregate_key_type = "IP"
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "rate-limit-per-ip"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "aws-ip-reputation"
    priority = 10
    override_action {
      none {}
    }
    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesAmazonIpReputationList"
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "aws-ip-reputation"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "aws-common"
    priority = 20
    override_action {
      none {}
    }
    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesCommonRuleSet"
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "aws-common"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "aws-known-bad-inputs"
    priority = 30
    override_action {
      none {}
    }
    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesKnownBadInputsRuleSet"
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "aws-known-bad-inputs"
      sampled_requests_enabled   = true
    }
  }

  dynamic "rule" {
    for_each = var.enable_waf_bot_control ? [1] : []
    content {
      name     = "aws-bot-control"
      priority = 40
      override_action {
        none {}
      }
      statement {
        managed_rule_group_statement {
          vendor_name = "AWS"
          name        = "AWSManagedRulesBotControlRuleSet"
        }
      }
      visibility_config {
        cloudwatch_metrics_enabled = true
        metric_name                = "aws-bot-control"
        sampled_requests_enabled   = true
      }
    }
  }

  visibility_config {
    cloudwatch_metrics_enabled = true
    metric_name                = "${local.name}-registry"
    sampled_requests_enabled   = true
  }
}

resource "aws_wafv2_web_acl_association" "stage" {
  resource_arn = aws_api_gateway_stage.registry.arn
  web_acl_arn  = aws_wafv2_web_acl.registry.arn
}

resource "aws_cloudwatch_log_group" "waf" {
  # WAF requires the aws-waf-logs- prefix.
  name              = "aws-waf-logs-${local.name}-registry"
  retention_in_days = var.log_retention_days
}

resource "aws_wafv2_web_acl_logging_configuration" "registry" {
  resource_arn            = aws_wafv2_web_acl.registry.arn
  log_destination_configs = [aws_cloudwatch_log_group.waf.arn]

  redacted_fields {
    single_header {
      name = "authorization"
    }
  }
  redacted_fields {
    single_header {
      name = "cookie"
    }
  }
}
