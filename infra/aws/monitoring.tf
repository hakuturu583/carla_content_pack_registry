resource "aws_sns_topic" "alarms" {
  name              = "${local.name}-registry-alarms"
  kms_master_key_id = "alias/aws/sns"
}

resource "aws_sns_topic_subscription" "email" {
  count     = var.alarm_email != "" ? 1 : 0
  topic_arn = aws_sns_topic.alarms.arn
  protocol  = "email"
  endpoint  = var.alarm_email
}

resource "aws_cloudwatch_metric_alarm" "lambda_errors" {
  alarm_name          = "${local.name}-registry-lambda-errors"
  namespace           = "AWS/Lambda"
  metric_name         = "Errors"
  dimensions          = { FunctionName = aws_lambda_function.registry.function_name }
  statistic           = "Sum"
  period              = 300
  evaluation_periods  = 1
  threshold           = 5
  comparison_operator = "GreaterThanOrEqualToThreshold"
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
}

resource "aws_cloudwatch_metric_alarm" "lambda_throttles" {
  alarm_name          = "${local.name}-registry-lambda-throttles"
  namespace           = "AWS/Lambda"
  metric_name         = "Throttles"
  dimensions          = { FunctionName = aws_lambda_function.registry.function_name }
  statistic           = "Sum"
  period              = 300
  evaluation_periods  = 1
  threshold           = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
}

resource "aws_cloudwatch_metric_alarm" "api_5xx" {
  alarm_name          = "${local.name}-registry-api-5xx"
  namespace           = "AWS/ApiGateway"
  metric_name         = "5XXError"
  dimensions          = { ApiName = aws_api_gateway_rest_api.registry.name, Stage = aws_api_gateway_stage.registry.stage_name }
  statistic           = "Sum"
  period              = 300
  evaluation_periods  = 1
  threshold           = 10
  comparison_operator = "GreaterThanOrEqualToThreshold"
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
}

resource "aws_cloudwatch_metric_alarm" "waf_blocked" {
  alarm_name          = "${local.name}-registry-waf-blocked-spike"
  namespace           = "AWS/WAFV2"
  metric_name         = "BlockedRequests"
  dimensions          = { WebACL = aws_wafv2_web_acl.registry.name, Region = var.region, Rule = "ALL" }
  statistic           = "Sum"
  period              = 300
  evaluation_periods  = 1
  threshold           = 1000
  comparison_operator = "GreaterThanOrEqualToThreshold"
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
}

# Authorization denials and storage failures, from the application's structured audit logs.
resource "aws_cloudwatch_log_metric_filter" "denied" {
  name           = "${local.name}-registry-denied"
  log_group_name = aws_cloudwatch_log_group.lambda.name
  pattern        = "{ $.msg = \"audit\" && $.outcome = \"deny\" }"
  metric_transformation {
    name      = "AuthorizationDenied"
    namespace = "CCPR/${var.environment}"
    value     = "1"
  }
}

resource "aws_budgets_budget" "monthly" {
  name         = "${local.name}-registry-monthly"
  budget_type  = "COST"
  limit_amount = tostring(var.monthly_budget_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  cost_filter {
    name   = "TagKeyValue"
    values = ["user:app$carla-content-pack-registry"]
  }

  dynamic "notification" {
    for_each = var.alarm_email != "" ? [80, 100] : []
    content {
      comparison_operator        = "GREATER_THAN"
      threshold                  = notification.value
      threshold_type             = "PERCENTAGE"
      notification_type          = "FORECASTED"
      subscriber_email_addresses = [var.alarm_email]
    }
  }
}
