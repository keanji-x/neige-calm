"""Reader-facing research summaries; operational detail stays in the status tool."""
from .series import realized_volatility


def table(columns, rows, caption):
    return {"columns": [{"key": k, "label": label} for k, label in columns],
            "rows": rows, "caption": caption}


def status_table(state):
    result = state.get("result")
    return table([("metric", "项目"), ("value", "状态")], [
        {"metric": "后台更新", "value": "启用" if state["enabled"] else "停止"},
        {"metric": "本次运行", "value": state["phase"]},
        {"metric": "每日时间", "value": f"{state['config']['update_hour_utc']:02}:00 UTC"},
        {"metric": "数据截止日", "value": result["as_of"] if result else "尚无成功结果"},
        {"metric": "最近成功时间", "value": state.get("last_success") or "尚无"},
        {"metric": "最近错误", "value": state.get("error") or "无"},
    ], "Barra 风格价格因子子集，非 MSCI 官方模型。失败时保留旧结果，请核对截止日。")


def overview_table(state):
    result = state.get("result")
    columns = [("forecast", "预测年化波动"), ("realized", "近 21 日实现波动"), ("calibration", "风险校准 RMS")]
    if result is None:
        message = "首次计算尚未完成" if state["phase"] != "failed" else "首次计算未成功，请检查数据源。"
        return table(columns, [{"forecast": "—", "realized": "—", "calibration": "—"}], message)
    realized = realized_volatility(result["history"], len(result["history"]) - 1)
    source = "离线回放" if result["source"].startswith("CSV replay") else (
        "Yahoo" if result["source"].startswith("Yahoo") else "其他数据源")
    caption = f"截至 {result['as_of']} · {len(result['exposures'])} 只美股 · 等权参考组合 · {source} · {result['run_id'][:6]}"
    if state["phase"] in ("failed", "interrupted"):
        caption += " · 更新未完成，部分图表可能属于不同批次"
    elif state["phase"] in ("queued", "running", "publishing"):
        caption += " · 更新中，当前仍为上次结果"
    elif not state["enabled"]:
        caption += " · 自动更新已暂停"
    return table(columns, [{
        "forecast": f"{result['portfolio']['predicted_annual_volatility'] * 100:.1f}%",
        "realized": f"{realized:.1f}%" if realized is not None else "—",
        "calibration": f"{result['validation']['rms_standardized_return']:.2f}",
    }], caption)


def leaders_table(result):
    rows = []
    for field, label in [("beta_z", "Beta · 市场敏感度"), ("momentum_z", "Momentum · 动量"),
                         ("residual_volatility_z", "ResidualVol · 残差波动")]:
        ranked = sorted(result["exposures"], key=lambda row: (row[field], row["symbol"]))
        describe = lambda values: " / ".join(f"{r['symbol']} {r[field]:+.2f}" for r in values)
        rows.append({"factor": label, "high": describe(list(reversed(ranked[-2:]))), "low": describe(ranked[:2])})
    return table([("factor", "风格"), ("high", "高暴露"), ("low", "低暴露")], rows,
                 f"{result['as_of']} · {result['run_id'][:6]} · 股票池内相对暴露 z，不是买卖评级。")


def result_tables(result):
    stamp = f"截止 {result['as_of']} · 结果 {result['run_id']} · {result['source']}"
    validation = result["validation"]
    portfolio = result["portfolio"]
    summary = [
        ("模型", result["model"]),
        ("研究股票数", len(result["exposures"])),
        ("预测年化波动率 (%)", round(portfolio["predicted_annual_volatility"] * 100, 3)),
        ("共同因子方差占比 (%)", round(portfolio["common_variance_fraction"] * 100, 3)),
        ("样本外检验天数", validation["observations"]),
        ("标准化收益 RMS (接近 1 仅为校准参考)", round(validation["rms_standardized_return"], 4)),
        ("收益落在 ±1.96σ 内 (%)", round(validation["within_1_96_sigma"] * 100, 3)),
        ("实际年化波动率 (%)", round(validation["realized_annual_volatility"] * 100, 3)),
        ("等权组合毛累计收益 (%)", round(validation["gross_cumulative_return"] * 100, 3)),
        ("等权组合最大回撤 (%)", round(validation["max_drawdown"] * 100, 3)),
        ("同期归因平均 R² (非预测能力)", round(validation["mean_attribution_r_squared"], 4)),
    ]
    exposures = [{"symbol": r["symbol"], **{k: round(r[k], 4) for k in (
        "beta", "momentum_z", "beta_z", "residual_volatility_z")},
        "residual_volatility_pct": round(r["residual_volatility"] * 100, 3)}
        for r in result["exposures"]]
    history = [{"date": r["date"], **{k: round(r[k] * 100, 4) for k in (
        "market", "beta", "momentum", "residual_volatility", "portfolio_return", "predicted_daily_volatility")},
        "equity": round(r["equal_weight_equity"], 6)} for r in reversed(result["history"])]
    return {
        "barra.leaders": leaders_table(result),
        "barra.validation": table([
            ("coverage", "±1.96σ 内的收益"), ("sessions", "样本外检验"), ("drawdown", "参考组合最大回撤"),
        ], [{"coverage": f"{validation['within_1_96_sigma'] * 100:.1f}%", "sessions": f"{validation['observations']} 个交易日",
             "drawdown": f"{validation['max_drawdown'] * 100:.1f}%"}],
            f"{result['as_of']} · {result['run_id'][:6]} · RMS 接近 1 是整体校准参考，不代表所有因子均有效。"),
        "barra.summary": table([("metric", "指标"), ("value", "值")],
                               [{"metric": k, "value": v} for k, v in summary], stamp +
                               " · 仅价格因子，未含行业与基本面；固定股票池存在选择及生存偏差，复权历史非时点数据。等权毛收益未计费用。"),
        "barra.exposures": table([
            ("symbol", "股票"), ("beta", "原始 Beta"), ("beta_z", "Beta 暴露 z"),
            ("momentum_z", "动量暴露 z"), ("residual_volatility_z", "残差波动暴露 z"),
            ("residual_volatility_pct", "年化残差波动 (%)"),
        ], exposures, stamp + " · z 为当前研究股票池内的截面标准化暴露，不是买卖评分。"),
        "barra.history": table([
            ("date", "日期"), ("market", "截距因子 (%)"), ("beta", "Beta 因子 (%)"),
            ("momentum", "动量因子 (%)"), ("residual_volatility", "波动因子 (%)"),
            ("portfolio_return", "等权毛收益 (%)"), ("predicted_daily_volatility", "事前日波动 (%)"),
            ("equity", "等权净值"),
        ], history, stamp + " · 风格列是标准化暴露的回归系数，不是可交易策略收益。每日等权再平衡，未计费用。"),
    }
