-- #2087 C1: a track recipe names a plugin tool by the minted name the model sees
-- (`plugin_<id>_<tool>`, every character of <id> and <tool> outside [A-Za-z0-9_] written `_`),
-- never by the raw upstream name its manifest declares. The closed set is every native tool of
-- the repository's three grandfathered plugins (`plugins/market`, `plugins/barra`,
-- `plugins/paper-trading`); the built-in names were rewritten by 0148 and `invest` declares words.
-- The scan is 0148's, longest name first, with one more refusal before a name: a raw name preceded
-- by `/` is a `neige://plugin/<id>/<raw>` URI segment (a live source a report cites), a protocol
-- id that stays. Identifier continuations stay unchanged and trailing sentence dots are kept as
-- punctuation. A minted name contains no raw name, so a rerun changes nothing. Each changed row
-- bumps `revision` and `updated_at`.
WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'market.holdings.list', 'plugin_dev_neige_market_market_holdings_list'),
  (2, 'market.holdings.set', 'plugin_dev_neige_market_market_holdings_set'),
  (3, 'market.series', 'plugin_dev_neige_market_market_series'),
  (4, 'market.quote', 'plugin_dev_neige_market_market_quote'),
  (5, 'barra.refresh', 'plugin_dev_neige_barra_barra_refresh'),
  (6, 'barra.series', 'plugin_dev_neige_barra_barra_series'),
  (7, 'barra.status', 'plugin_dev_neige_barra_barra_status'),
  (8, 'barra.start', 'plugin_dev_neige_barra_barra_start'),
  (9, 'barra.stop', 'plugin_dev_neige_barra_barra_stop'),
  (10, 'spy.execute', 'plugin_dev_neige_paper_trading_spy_execute'),
  (11, 'spy.refresh', 'plugin_dev_neige_paper_trading_spy_refresh'),
  (12, 'spy.status', 'plugin_dev_neige_paper_trading_spy_status'),
  (13, 'spy.plan', 'plugin_dev_neige_paper_trading_spy_plan')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1, body, '', ''
    FROM track_recipes
   WHERE instr(body, 'market.') > 0
      OR instr(body, 'barra.') > 0
      OR instr(body, 'spy.') > 0
  UNION ALL
  SELECT id,
         CASE WHEN instr(rest, old) = 0 THEN scan.step + 1 ELSE scan.step END,
         CASE WHEN instr(rest, old) = 0 THEN output || rest
              ELSE substr(rest, instr(rest, old) + length(old)) END,
         CASE WHEN instr(rest, old) = 0 THEN ''
              ELSE output || substr(rest, 1, instr(rest, old) - 1) ||
                CASE WHEN
                  (CASE WHEN instr(rest, old) = 1 THEN previous
                        ELSE substr(rest, instr(rest, old) - 1, 1) END)
                    NOT GLOB '[A-Za-z0-9_./-]'
                  AND substr(rest, instr(rest, old) + length(old), 1)
                    NOT GLOB '[A-Za-z0-9_-]'
                  AND substr(ltrim(substr(rest, instr(rest, old) + length(old)), '.'), 1, 1)
                    NOT GLOB '[A-Za-z0-9_.-]'
                THEN new ELSE old END END,
         CASE WHEN instr(rest, old) = 0 THEN '' ELSE substr(old, -1) END
    FROM scan JOIN names ON names.step = scan.step
)
UPDATE track_recipes
   SET body = scan.rest,
       revision = track_recipes.revision + 1,
       updated_at = MAX(track_recipes.updated_at + 1,
         CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
  FROM scan
 WHERE scan.step = 14 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
