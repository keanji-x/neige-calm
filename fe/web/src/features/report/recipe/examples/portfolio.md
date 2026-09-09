<!-- 通过聊天维护这份 Report 的 layout 区块。布局、字段与样式配置保存在 Report 中；行情引用当前 Track 的 Market data 插件。登记持仓调用现有 market 工具；交易日志只记录用户说明的已发生交易。使用真实区块版本修改，不用行情覆盖 Report。研究链接和下次事件填写持仓表 annotations；其 keys 为 venue + asset。计价币种默认 CNY，改币种时同步图表 unit.equals，不转换或编造历史。新增模板时保留空 annotations/交易 rows，不复制私人持仓和 Track ID。 -->

# 组合概览

```neige-block layout
{
  "version": 1,
  "columns": 2,
  "gap": "wide",
  "surface": "plain",
  "items": [
    {
      "kind": "chart",
      "title": "组合走势",
      "span": 1,
      "data": {
        "source": "neige://plugin/dev-neige-market/portfolio.history"
      },
      "chart": "line",
      "x": "at",
      "y": "total",
      "height": 260,
      "color": "#4a5f9b",
      "unit": {
        "key": "currency",
        "equals": "CNY"
      },
      "ranges": [
        30,
        90,
        180
      ],
      "defaultRange": 180
    },
    {
      "kind": "chart",
      "title": "持仓权重",
      "span": 1,
      "data": {
        "source": "neige://plugin/dev-neige-market/portfolio.holdings"
      },
      "exclude": {
        "key": "asset",
        "value": "Total"
      },
      "chart": "donut",
      "labelSuffixKey": "venue",
      "x": "asset",
      "y": "value",
      "height": 220,
      "color": "#4a5f9b",
      "unit": {
        "key": "currency",
        "equals": "CNY",
        "row": {
          "key": "asset",
          "value": "Total"
        }
      }
    }
  ]
}
```

# 持仓明细

```neige-block layout
{
  "version": 1,
  "columns": 1,
  "gap": "wide",
  "surface": "plain",
  "items": [
    {
      "kind": "table",
      "title": "",
      "span": 1,
      "data": {
        "source": "neige://plugin/dev-neige-market/portfolio.holdings",
        "annotations": {
          "keys": [
            "venue",
            "asset"
          ],
          "rows": []
        }
      },
      "exclude": {
        "key": "asset",
        "value": "Total"
      },
      "total": {
        "row": {
          "key": "asset",
          "value": "Total"
        },
        "key": "value"
      },
      "columns": [
        {
          "key": "name",
          "label": "股票 / 研究档案",
          "format": "text",
          "digits": 0,
          "fallbackKey": "asset",
          "suffixKey": "venue",
          "linkKey": "track"
        },
        {
          "key": "price",
          "label": "现价",
          "format": "number",
          "digits": 8,
          "minDigits": 2,
          "suffixKey": "currency"
        },
        {
          "key": "change",
          "label": "当日涨跌",
          "format": "percent",
          "digits": 2
        },
        {
          "key": "value",
          "label": "仓位",
          "format": "share",
          "digits": 1
        },
        {
          "key": "nextEvent",
          "label": "下次事件",
          "format": "text",
          "digits": 0
        }
      ]
    }
  ]
}
```

# 交易日志

```neige-block layout
{
  "version": 1,
  "columns": 1,
  "gap": "wide",
  "surface": "plain",
  "items": [
    {
      "kind": "table",
      "title": "",
      "span": 1,
      "data": {
        "rows": []
      },
      "columns": [
        {
          "key": "date",
          "label": "日期",
          "format": "text",
          "digits": 0
        },
        {
          "key": "name",
          "label": "股票",
          "format": "text",
          "digits": 0,
          "linkKey": "track"
        },
        {
          "key": "side",
          "label": "操作",
          "format": "text",
          "digits": 0
        },
        {
          "key": "quantity",
          "label": "数量",
          "format": "number",
          "digits": 4
        },
        {
          "key": "price",
          "label": "成交价",
          "format": "number",
          "digits": 8,
          "minDigits": 2,
          "suffixKey": "currency"
        },
        {
          "key": "reason",
          "label": "当时的理由",
          "format": "text",
          "digits": 0
        }
      ]
    }
  ]
}
```
