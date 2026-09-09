<!-- 通过聊天维护这份 Report。布局、字段与样式保存在 layout 区块，现金和证券数量保存在当前 Track 的 Market data 插件中。登记当前现金余额使用 market.cash.set，确认保存使用 market.cash.list；这是绝对余额，支持 CNY/USD/HKD、最多两位小数，零余额也保留。登记证券使用 market.holdings.set/list。普通金额或数量录入只调用数据工具，不改 Report 布局、添加现金说明段落或写交易日志；不要将现金伪装成证券，也不要因登记交易而自动扣减现金。只有用户明确要求修改布局、研究链接、事件或日志时才使用真实区块版本修改 Report。研究链接和下次事件填写证券表 annotations，keys 为 venue + asset。图表展示包含已登记现金的组合，默认计价 CNY；不为单个 Track 的登记请求修改插件全局计价设置，不转换或编造旧历史。新增模板保留空 annotations/交易 rows，不复制私人现金、持仓或 Track ID。 -->

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
        "source": "neige://plugin/dev-neige-market/portfolio.total_history"
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
        "source": "neige://plugin/dev-neige-market/portfolio.allocation"
      },
      "exclude": {
        "key": "kind",
        "value": "total"
      },
      "chart": "donut",
      "x": "label",
      "y": "value",
      "height": 220,
      "color": "#4a5f9b",
      "unit": {
        "key": "currency",
        "equals": "CNY",
        "row": {
          "key": "kind",
          "value": "total"
        }
      }
    },
    {
      "kind": "table",
      "title": "现金余额",
      "span": 2,
      "data": {
        "source": "neige://plugin/dev-neige-market/portfolio.cash"
      },
      "columns": [
        {
          "key": "currency",
          "label": "币种",
          "format": "text",
          "digits": 0
        },
        {
          "key": "amount",
          "label": "余额",
          "format": "number",
          "digits": 2
        },
        {
          "key": "value",
          "label": "折合金额",
          "format": "number",
          "digits": 2,
          "suffixKey": "value_currency"
        },
        {
          "key": "weight",
          "label": "仓位",
          "format": "percent",
          "digits": 1
        }
      ]
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
        "source": "neige://plugin/dev-neige-market/portfolio.positions",
        "annotations": {
          "keys": [
            "venue",
            "asset"
          ],
          "rows": []
        }
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
          "key": "weight",
          "label": "仓位",
          "format": "percent",
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
