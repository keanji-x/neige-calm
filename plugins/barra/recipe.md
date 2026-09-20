<!-- neige:contract {"version":1,"sections":[{"h1":"美股风格与风险"},{"h1":"风格轮动"},{"h1":"暴露最突出的股票"},{"h1":"预测风险与实际波动"},{"h1":"等权参考组合"},{"h1":"口径"}]} -->
<!-- Planner: 普通 Recipe，无 template_id/forge 绑定。用户确认后在当前 Track 调用 barra.start，使用已安装的 dev-neige-barra 插件。方法是公开价格因子子集，不是 MSCI 官方模型或投资建议。数值和图表均由插件维护，不要抄写动态数据，不要加运行日志、任务回顾或免责声明大段前言。正常运行时不显示技术状态；更新异常由概览提示。详细运维信息通过 barra.status 查询。不要把因子回归系数当作可交易策略收益。归档或删除 Track 前调用 barra.stop。 -->

# 美股风格与风险

```neige-block table
{"source":"neige://plugin/dev-neige-barra/barra.overview"}
```

# 风格轮动

```neige-block chart.series
{"source":"neige://plugin/dev-neige-barra/barra.series","series":["BARRA:Beta","BARRA:Momentum","BARRA:ResidualVol"],"field":"close","range":"6M","period":"day","view":"line","caption":"窗口内日因子收益累加，单位：百分点。Beta 为市场敏感度，Momentum 为动量，ResidualVol 为残差波动；这些是回归系数，不是策略净值。"}
```

# 暴露最突出的股票

```neige-block table
{"source":"neige://plugin/dev-neige-barra/barra.leaders"}
```

# 预测风险与实际波动

```neige-block chart.series
{"source":"neige://plugin/dev-neige-barra/barra.series","series":["BARRA:Forecast","BARRA:Realized21D"],"field":"close","range":"6M","period":"day","view":"line","caption":"年化波动率（%）。Forecast 使用前一日可得信息；Realized21D 为截至当日的 21 日实现波动率，不是未来 21 日预测检验。"}
```

```neige-block table
{"source":"neige://plugin/dev-neige-barra/barra.validation"}
```

# 等权参考组合

```neige-block chart.series
{"source":"neige://plugin/dev-neige-barra/barra.series","series":["BARRA:EqualWeight"],"field":"close","range":"6M","period":"day","view":"line","caption":"研究期起点为 100；每日等权再平衡，未扣交易费用。固定股票池有选择与生存偏差，不代表可执行策略。"}
```

# 口径

252 日 Beta、跳过最近 21 日的动量与残差波动，昨日暴露归因今日收益。公开的 Barra 风格价格因子子集，非 MSCI 官方模型；暂不含行业、规模和估值。数据来自当前版本的复权历史，非时点归档；等权组合的风险校准不验证所有风格方向。
