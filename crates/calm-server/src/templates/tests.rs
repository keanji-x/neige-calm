use super::*;
use calm_types::report_contract::check_document;
use calm_types::track_report::{research_header, work_brief_header};
use std::collections::BTreeSet;

#[test]
fn capability_builtin_templates_remain_disabled_including_roster_copies() {
    let roster = TemplateRoster::builtin();
    for roster in [roster, &roster.copy_of_entries()] {
        for template in roster.entries() {
            assert!(template.capabilities().is_empty(), "{}", template.key());
        }
    }
}

/// Plan templates with a shared working-method contract; other entries use their own briefs.
const PLAN_TEMPLATES: [&str; 3] = [DEV, SMALL_CHANGE, INVESTIGATION];

fn roster() -> &'static TemplateRoster {
    TemplateRoster::builtin()
}

fn body(key: &str) -> String {
    roster()
        .get(key)
        .unwrap_or_else(|| panic!("`{key}` is not on the builtin roster"))
        .recipe()
        .body
}

#[test]
fn the_key_constants_are_exactly_the_builtin_file_ids() {
    let ids: Vec<&str> = roster().entries().iter().map(Template::key).collect();
    assert_eq!(
        ids,
        [
            DAILY_PLANNER,
            DEV,
            LONG_TERM_GOAL,
            SMALL_CHANGE,
            INVESTIGATION,
            INVESTMENT_RESEARCH,
            "opencode-history-rebuild"
        ]
    );
}

#[test]
fn builtin_directory_and_roster_are_the_same_set() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/builtin");
    let mut stems = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("templates/builtin exists") {
        let path = entry.expect("read_dir entry").path();
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("md"),
            "only template files live in templates/builtin: {}",
            path.display()
        );
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("utf-8 stem")
            .to_string();
        let text = std::fs::read_to_string(&path).expect("read template file");
        let (front, file_body) = front_matter::parse(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(
            front.id,
            stem,
            "{}: front-matter id is its stem",
            path.display()
        );
        let template = roster()
            .get(&stem)
            .unwrap_or_else(|| panic!("{}: on disk but not on the roster", path.display()));
        assert_eq!(template.title(), front.title, "{stem}: title");
        assert_eq!(
            template.recipe().body,
            file_body,
            "{stem}: the recipe body is the file's bytes after the front matter"
        );
        assert!(stems.insert(stem));
    }
    let roster_ids: BTreeSet<String> = roster()
        .entries()
        .iter()
        .map(|template| template.key().to_string())
        .collect();
    assert_eq!(
        stems, roster_ids,
        "templates/builtin/*.md stems == roster ids"
    );
}

#[test]
#[should_panic(expected = "duplicate template id `twin`")]
fn duplicate_ids_panic_at_first_use() {
    TemplateRoster::load(&[
        "+++\nid = \"twin\"\ntitle = \"A\"\n+++\n# A\n",
        "+++\nid = \"twin\"\ntitle = \"B\"\n+++\n# B\n",
    ]);
}

#[test]
#[should_panic(expected = "template source #1: template file must open with a `+++`")]
fn a_source_without_front_matter_panics_at_first_use() {
    TemplateRoster::load(&[
        "+++\nid = \"fine\"\ntitle = \"Fine\"\n+++\n# A\n",
        "# no front matter\n",
    ]);
}

#[test]
fn known_keys_round_trip() {
    for (index, template) in roster().entries().iter().enumerate() {
        let key = template.key();
        let found = roster().get(key).expect("roster key admits");
        assert!(std::ptr::eq(found, &roster().entries()[index]));
        let recipe = template.recipe();
        assert_eq!(
            recipe.summary,
            template.title(),
            "{key}: summary is the title"
        );
        assert!(!recipe.summary.is_empty(), "{key}: empty recipe summary");
        assert!(!recipe.body.is_empty(), "{key}: empty recipe body");
    }
    assert!(roster().get("missing-template").is_none());
    assert!(roster().get("").is_none());
}

/// Asserted by data-pointer identity: an equality assertion would pass for a copy too.
#[test]
fn get_returns_the_rosters_own_borrow() {
    for (index, template) in roster().entries().iter().enumerate() {
        let owned = String::from(template.key);
        assert_ne!(
            owned.as_ptr(),
            template.key.as_ptr(),
            "the fixture must not accidentally be the roster's own buffer"
        );
        let found = roster().get(owned.as_str()).expect("roster key admits");
        assert!(
            std::ptr::eq(found, &roster().entries()[index]),
            "get must borrow the roster entry, not rebuild one"
        );
        assert!(
            std::ptr::eq(found.key.as_ptr(), template.key.as_ptr()),
            "the admitted key must be the roster's &'static str, not the caller's"
        );
    }
}

/// Accessor-against-private-field comparison; only expressible in the defining module.
#[test]
fn the_accessors_hand_back_the_roster_fields_own_buffer() {
    for template in roster().entries() {
        assert!(
            std::ptr::eq(template.key().as_ptr(), template.key.as_ptr()),
            "`{}`: key() must be the `key` field's own buffer",
            template.key
        );
        assert!(
            std::ptr::eq(template.title().as_ptr(), template.title.as_ptr()),
            "`{}`: title() must be the `title` field's own buffer",
            template.key
        );
        let recipe = template.recipe();
        assert_eq!(
            recipe.summary, template.title,
            "`{}`: summary",
            template.key
        );
        assert_eq!(recipe.body, template.body, "`{}`: body", template.key);
    }
}

#[test]
fn every_work_template_carries_the_contract_and_method_without_tasks() {
    let default_body = TrackReportPayload::initial().body;
    let default_block = split_body(&default_body)[0].raw.clone();
    let shared = default_block.strip_suffix("-->\n\n").unwrap();
    for key in PLAN_TEMPLATES {
        let report = roster().get(key).unwrap().recipe();
        assert_eq!(check_document(&report.body), Ok(Some(work_brief_header())));
        let slices = split_body(&report.body);
        assert!(
            slices[0].raw.starts_with(shared),
            "{key}: shared report contract"
        );
        assert!(
            slices[0].raw.ends_with("-->\n\n"),
            "{key}: closed instructions"
        );
        assert!(
            slices[0].raw.contains("Working method"),
            "{key}: startup method"
        );
        assert!(
            template_task_payloads_from_body(&report.body).is_empty(),
            "{key}"
        );
        let headings: Vec<_> = slices[1..]
            .iter()
            .map(|s| s.raw.lines().next().unwrap())
            .collect();
        assert_eq!(
            headings,
            ["# 概要", "# 待你定", "# 已完成", "# 决策"],
            "{key}"
        );
    }
}

#[test]
fn investment_research_h1s_are_the_research_header_sections() {
    let body = body(INVESTMENT_RESEARCH);
    let slices = split_body(&body);
    let h1s: Vec<String> = slices[1..]
        .iter()
        .map(|slice| {
            slice
                .raw
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("# "))
                .unwrap_or_else(|| panic!("block does not start with an H1: {:?}", slice.raw))
                .to_string()
        })
        .collect();
    let declared: Vec<String> = research_header()
        .sections
        .into_iter()
        .map(|section| section.h1)
        .collect();
    assert_eq!(h1s, declared);
    assert_eq!(
        check_document(&body).unwrap().unwrap().sections,
        research_header().sections,
        "and the header the body carries is the one the constructor builds"
    );
}

#[test]
fn investment_research_is_a_task_less_research_skeleton() {
    let report = roster()
        .get(INVESTMENT_RESEARCH)
        .expect("research template")
        .recipe();
    assert_eq!(report.summary, "Investment research");
    assert_eq!(
        check_document(&report.body),
        Ok(Some(research_header())),
        "must lead with the closed research contract, not the work-brief one"
    );
    assert!(report.report_startup_read_required());
    // It reads as written because of its birth summary, not its body: the same body with an
    // empty summary is structurally unwritten.
    assert!(
        !TrackReportPayload::new("", report.body.clone()).report_startup_read_required(),
        "the empty research skeleton is unwritten by shape; only the summary makes it written"
    );

    let slices = split_body(&report.body);
    assert!(
        slices[0].raw.ends_with("-->\n\n"),
        "the research contract must be its own closed block, got {:?}",
        slices[0].raw
    );
    let heads: Vec<&str> = slices[1..]
        .iter()
        .map(|slice| slice.raw.lines().next().unwrap_or(""))
        .collect();
    assert_eq!(
        heads,
        [
            "# 结论",
            "# 待你定",
            "# 核心逻辑",
            "# 关键数据",
            "# 风险与证伪",
            "# 催化剂与跟踪",
            "# 来源与边界",
        ],
        "exactly the seven research H1s, in order, after the contract block"
    );
    for slice in &slices[1..] {
        assert!(
            slice.raw.lines().skip(1).all(|line| line.trim().is_empty()),
            "sections start empty: {:?}",
            slice.raw
        );
    }
    assert!(
        slices.iter().all(|s| parse_fence(&s.raw).is_none()),
        "no fences of any kind in the skeleton"
    );
    assert!(template_task_payloads_from_body(&report.body).is_empty());
    assert!(!report.body.contains("# Plan"));
}

#[test]
fn builtin_templates_contain_no_task_declarations() {
    for template in roster().entries() {
        assert!(
            template_task_payloads_from_body(&template.recipe().body).is_empty(),
            "{} must supply working instructions, not placeholder tasks",
            template.key()
        );
    }
}

#[test]
fn body_prose_and_foreign_fences_are_skipped_not_parsed() {
    let mut body = body(SMALL_CHANGE);
    let before = template_task_payloads_from_body(&body).len();
    body.push_str("\n## Notes\n\nSomething the user typed.\n\n");
    body.push_str("```neige-block table\n{\n  \"rows\": []\n}\n```\n");
    body.push_str("```neige-block task\nnot json\n```\n");
    assert_eq!(template_task_payloads_from_body(&body).len(), before);
}

#[test]
fn long_term_goal_planner_context_roster_loads_its_contract_sections() {
    let template = roster()
        .get(LONG_TERM_GOAL)
        .expect("long-term-goal on roster");
    assert!(template.user_creatable());
    assert_eq!(template.title(), "长期目标");
    let compiled = crate::routes::tracks::compile_template(template).unwrap();
    let contract = check_document(compiled.body()).unwrap().unwrap();
    assert_eq!(
        contract
            .sections
            .iter()
            .map(|s| s.h1.as_str())
            .collect::<Vec<_>>(),
        ["规范", "需要你", "目标", "关键结果", "Roadmap", "已交付"]
    );
    assert!(contract.sections[1].omit_if_empty);
    assert_eq!(
        compiled
            .body()
            .lines()
            .filter(|line| line.starts_with("# "))
            .collect::<Vec<_>>(),
        ["# 规范", "# 目标", "# 关键结果", "# Roadmap", "# 已交付"]
    );
    assert!(template_task_payloads_from_body(compiled.body()).is_empty());
    assert!(!compiled.body().contains("neige:input-form"));
    assert!(
        compiled
            .body()
            .contains("neige://plugin/kernel/planner.context")
    );
}
