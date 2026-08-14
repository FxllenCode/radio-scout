//! The curation surface (#49, spec US 45–46) driven the way an Operator's
//! browser drives it: a real session in a cookie jar, a CSRF token on every
//! write, and JSON over the wire.
//!
//! What is asserted here is what an Operator could observe — a status, a body, a
//! row that changed, a Call that stopped being ingestible. The pure halves
//! (validation, the blacklist algebra, the patch semantics) are unit-tested in
//! `src/curate/`; this file is for the wiring, the guard and the dialect.

mod common;

use common::{CallUpload, TestApp};
use radio_scout::db::entities::{call, group, system, tag, talkgroup, unit};
use rstest::rstest;
use serde_json::{Value, json};

/// Every row of a listing, by whatever field names it.
fn names(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .expect("a listing")
        .iter()
        .map(|row| row["name"].as_str().expect("a name").to_string())
        .collect()
}

/// The `error` slug of a refusal.
fn slug(body: &Value) -> &str {
    body["error"].as_str().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The guard
// ---------------------------------------------------------------------------

/// Every curation route sits under the prefix layer (#19), so a caller with no
/// session is turned away before any handler runs — including the reads, which
/// reveal an Instance's configuration.
///
/// Written as a table over **every** route rather than a spot check: the whole
/// point of the prefix layer is that a route added beside the others is gated by
/// construction, and this is what would fail if one were mounted outside it.
#[tokio::test]
async fn no_session_reaches_no_curation_route() {
    let app = TestApp::spawn().await;

    for path in [
        "/api/admin/systems",
        "/api/admin/talkgroups",
        "/api/admin/groups",
        "/api/admin/tags",
        "/api/admin/units",
        "/api/admin/api-keys",
        // #50's merge curation, which reads and rewrites the archive — the two
        // routes it would be worst to have mounted outside the layer.
        "/api/admin/talkgroups/1/members",
        "/api/admin/units/1/ranges",
        // #51's document — the read is the whole configuration and the write
        // rewrites it, so this is the pair it would be worst to leave open.
        "/api/admin/config",
        "/api/admin/config/import",
    ] {
        let response = app.get(path).await;
        assert_eq!(response.status(), 401, "GET {path}");

        for method in [
            reqwest::Method::POST,
            reqwest::Method::PATCH,
            reqwest::Method::DELETE,
        ] {
            let refused = app
                .admin_verb(method.clone(), path, None)
                .send()
                .await
                .expect("a request");
            assert_eq!(refused.status(), 401, "{method} {path}");
        }
    }
}

/// A live session still may not *write* without echoing its CSRF token — the
/// synchronizer token covers `PATCH` and `DELETE`, not only the `POST` #19
/// shipped with.
#[tokio::test]
async fn a_write_without_the_csrf_token_is_refused() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, created) = app
        .admin_post("/api/admin/groups", json!({"name": "Fire"}))
        .await;
    let id = created["id"].as_i64().expect("an id");

    for (method, path) in [
        (reqwest::Method::POST, "/api/admin/groups".to_string()),
        (reqwest::Method::PATCH, format!("/api/admin/groups/{id}")),
        (reqwest::Method::DELETE, format!("/api/admin/groups/{id}")),
    ] {
        let refused = app
            .admin_verb(method.clone(), &path, None)
            .json(&json!({"name": "Law"}))
            .send()
            .await
            .expect("a request");

        assert_eq!(refused.status(), 403, "{method} {path}");
    }
    // ...and nothing was written by any of them.
    assert_eq!(app.count::<group::Entity>().await, 1);
}

// ---------------------------------------------------------------------------
// Groups and Tags — the name-only entities
// ---------------------------------------------------------------------------

/// The whole life of a Group: created, listed, renamed, deleted.
#[tokio::test]
async fn a_group_is_created_listed_renamed_and_deleted() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, created) = app
        .admin_post("/api/admin/groups", json!({"name": "  Fire  "}))
        .await;
    assert_eq!(status, 201);
    // Trimmed on the way in, so " Fire " and "Fire" are never two Groups.
    assert_eq!(created["name"], "Fire");
    assert_eq!(created["talkgroups"], 0);
    let id = created["id"].as_i64().expect("an id");

    let (status, listed) = app.admin_get("/api/admin/groups").await;
    assert_eq!(status, 200);
    assert_eq!(names(&listed), vec!["Fire"]);

    let (status, renamed) = app
        .admin_patch(
            &format!("/api/admin/groups/{id}"),
            json!({"name": "Fire/EMS"}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(renamed["name"], "Fire/EMS");
    assert_eq!(renamed["id"], id);

    let (status, _) = app.admin_delete(&format!("/api/admin/groups/{id}")).await;
    assert_eq!(status, 204);
    assert_eq!(app.count::<group::Entity>().await, 0);
}

/// The same for a Tag, because two entities that behave identically should be
/// *proven* to, not assumed to.
#[tokio::test]
async fn a_tag_is_created_listed_renamed_and_deleted() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, created) = app
        .admin_post("/api/admin/tags", json!({"name": "Fire Dispatch"}))
        .await;
    assert_eq!(status, 201);
    let id = created["id"].as_i64().expect("an id");

    let (_, listed) = app.admin_get("/api/admin/tags").await;
    assert_eq!(names(&listed), vec!["Fire Dispatch"]);

    let (status, renamed) = app
        .admin_patch(
            &format!("/api/admin/tags/{id}"),
            json!({"name": "EMS Dispatch"}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(renamed["name"], "EMS Dispatch");

    let (status, _) = app.admin_delete(&format!("/api/admin/tags/{id}")).await;
    assert_eq!(status, 204);
    assert_eq!(app.count::<tag::Entity>().await, 0);
}

/// A listing says how many Talkgroups a Group or Tag holds, because that is what
/// an Operator about to delete one needs to know — and it is one query for the
/// whole list, never one per row.
#[tokio::test]
async fn a_listing_counts_the_talkgroups_behind_each_row() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    // Two Calls on two Talkgroups, auto-populated into the default Group and Tag.
    app.upload_ok(CallUpload::new().talkgroup(100)).await;
    app.upload_ok(CallUpload::new().talkgroup(200)).await;

    let (_, groups) = app.admin_get("/api/admin/groups").await;
    let (_, tags) = app.admin_get("/api/admin/tags").await;

    let group = &groups["results"][0];
    assert_eq!(group["talkgroups"], 2, "{groups}");
    assert_eq!(tags["results"][0]["talkgroups"], 2, "{tags}");
}

/// A name that is already taken is refused rather than silently merged — on
/// create and on rename alike, since a rename is the same collision arriving by
/// a different door.
#[tokio::test]
async fn a_duplicate_name_is_refused() {
    let app = TestApp::spawn().await;
    app.login().await;
    app.admin_post("/api/admin/groups", json!({"name": "Fire"}))
        .await;
    let (_, law) = app
        .admin_post("/api/admin/groups", json!({"name": "Law"}))
        .await;
    let law = law["id"].as_i64().expect("an id");

    let (status, refused) = app
        .admin_post("/api/admin/groups", json!({"name": "Fire"}))
        .await;
    assert_eq!(status, 409);
    assert_eq!(slug(&refused), "name-taken");

    let (status, refused) = app
        .admin_patch(&format!("/api/admin/groups/{law}"), json!({"name": "Fire"}))
        .await;
    assert_eq!(status, 409);
    assert_eq!(slug(&refused), "name-taken");
    // ...and renaming a row to the name it already has is not a collision with
    // itself.
    let (status, _) = app
        .admin_patch(&format!("/api/admin/groups/{law}"), json!({"name": "Law"}))
        .await;
    assert_eq!(status, 200);
}

/// A blank name is refused, naming the field — an empty Group is unreachable in
/// every panel that renders one.
#[tokio::test]
async fn a_blank_name_is_refused_by_name() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, refused) = app
        .admin_post("/api/admin/tags", json!({"name": "   "}))
        .await;

    assert_eq!(status, 400);
    assert_eq!(slug(&refused), "field-required");
    assert_eq!(refused["field"], "name");
}

/// A field nobody knows is a typo, and a typo that is ignored is a setting an
/// Operator believes they changed. rdio's importer coerces whatever it can and
/// drops the rest; this refuses.
#[tokio::test]
async fn an_unknown_field_is_refused_rather_than_ignored() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, _) = app
        .admin_post(
            "/api/admin/groups",
            json!({"name": "Fire", "colour": "red"}),
        )
        .await;

    assert!((400..500).contains(&status), "got {status}");
    assert_eq!(app.count::<group::Entity>().await, 0);
}

/// Deleting a Group takes its Talkgroup links with it and leaves the Talkgroups
/// themselves alone — a category is a way of looking at channels, not a thing
/// they belong to.
#[tokio::test]
async fn deleting_a_group_unlinks_its_talkgroups_and_keeps_them() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100)).await;
    let (_, groups) = app.admin_get("/api/admin/groups").await;
    let id = groups["results"][0]["id"].as_i64().expect("an id");

    let (status, _) = app.admin_delete(&format!("/api/admin/groups/{id}")).await;

    assert_eq!(status, 204);
    assert_eq!(app.count::<talkgroup::Entity>().await, 1);
    let catalog = app.get_json("/api/catalog").await;
    let groups = catalog["systems"][0]["talkgroups"][0]["groups"]
        .as_array()
        .expect("a group list");
    assert!(groups.is_empty(), "{catalog}");
}

/// Deleting a Tag un-tags its Talkgroups rather than deleting them — a
/// Talkgroup has exactly one Tag (CONTEXT.md) and losing it must not lose the
/// channel.
#[tokio::test]
async fn deleting_a_tag_untags_its_talkgroups_and_keeps_them() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100)).await;
    let (_, tags) = app.admin_get("/api/admin/tags").await;
    let id = tags["results"][0]["id"].as_i64().expect("an id");

    let (status, _) = app.admin_delete(&format!("/api/admin/tags/{id}")).await;

    assert_eq!(status, 204);
    assert_eq!(app.count::<talkgroup::Entity>().await, 1);
    let catalog = app.get_json("/api/catalog").await;
    assert!(
        catalog["systems"][0]["talkgroups"][0]["tag"].is_null(),
        "{catalog}"
    );
}

/// A row that is not there is a 404 on every verb that names one, rather than a
/// 500 or a silent success.
#[tokio::test]
async fn an_unknown_row_is_not_found() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, refused) = app
        .admin_patch("/api/admin/groups/999", json!({"name": "Fire"}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(slug(&refused), "group-not-found");

    let (status, refused) = app.admin_delete("/api/admin/tags/999").await;
    assert_eq!(status, 404);
    assert_eq!(slug(&refused), "tag-not-found");
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// A System's whole life, including the two fields nothing else in Radio-Scout
/// can set: the per-System auto-populate flag (#8) and the nullable enhancement
/// scope (#20), which shipped as columns with no way to reach them.
#[tokio::test]
async fn a_system_is_created_edited_and_deleted() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, created) = app
        .admin_post(
            "/api/admin/systems",
            json!({"ref": 11, "label": "Fulton", "autoPopulate": true}),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["ref"], 11);
    assert_eq!(created["label"], "Fulton");
    assert_eq!(created["autoPopulate"], true);
    assert!(created["enhancement"].is_null());
    assert_eq!(created["calls"], 0);
    let id = created["id"].as_i64().expect("an id");

    // A patch changes what it names and leaves the rest alone...
    let (status, edited) = app
        .admin_patch(
            &format!("/api/admin/systems/{id}"),
            json!({"enhancement": false}),
        )
        .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["enhancement"], false);
    assert_eq!(edited["label"], "Fulton", "untouched by this patch");
    assert_eq!(edited["autoPopulate"], true);

    // ...and `null` is how a nullable field goes back to inheriting, which an
    // absent field could never say.
    let (_, inherited) = app
        .admin_patch(
            &format!("/api/admin/systems/{id}"),
            json!({"enhancement": null, "label": null}),
        )
        .await;
    assert!(inherited["enhancement"].is_null(), "{inherited}");
    assert!(inherited["label"].is_null(), "{inherited}");

    let (status, _) = app.admin_delete(&format!("/api/admin/systems/{id}")).await;
    assert_eq!(status, 204);
    assert_eq!(app.count::<system::Entity>().await, 0);
}

/// A System created with no Ref is numbered the way #8 numbers one a recorder
/// identified by name alone: the lowest free one.
#[tokio::test]
async fn a_system_with_no_ref_is_given_the_lowest_free_one() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (_, first) = app
        .admin_post("/api/admin/systems", json!({"label": "A"}))
        .await;
    let (_, second) = app
        .admin_post("/api/admin/systems", json!({"label": "B"}))
        .await;

    assert_eq!(first["ref"], 1);
    assert_eq!(second["ref"], 2);
}

/// Two Systems answering to one Ref would be a recorder's upload landing on
/// either — refused rather than left to the unique index to report as a 500.
#[tokio::test]
async fn a_system_ref_cannot_be_taken_twice() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, first) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let id = first["id"].as_i64().expect("an id");

    let (status, refused) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    assert_eq!(status, 409);
    assert_eq!(slug(&refused), "system-ref-taken");

    // ...and renumbering onto an occupied Ref is the same collision by another
    // door, while renumbering onto its own is a form saved unchanged.
    app.admin_post("/api/admin/systems", json!({"ref": 12}))
        .await;
    let (status, refused) = app
        .admin_patch(&format!("/api/admin/systems/{id}"), json!({"ref": 12}))
        .await;
    assert_eq!(status, 409, "{refused}");
    let (status, _) = app
        .admin_patch(&format!("/api/admin/systems/{id}"), json!({"ref": 11}))
        .await;
    assert_eq!(status, 200);
}

/// The System listing counts what hangs off each row, so an Operator can see
/// what a delete is about to cost before they ask for it.
#[tokio::test]
async fn a_system_listing_counts_what_hangs_off_it() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100)).await;
    app.upload_ok(CallUpload::new().talkgroup(200)).await;

    let (_, listed) = app.admin_get("/api/admin/systems").await;

    let system = &listed["results"][0];
    assert_eq!(system["talkgroups"], 2, "{listed}");
    assert_eq!(system["calls"], 2, "{listed}");
}

// ---------------------------------------------------------------------------
// Deleting what still has Archive behind it
// ---------------------------------------------------------------------------

/// **The refusal that keeps a night of Archive.** Retention owns removing Calls,
/// so admin says how many there are and stops — rdio deletes the System and
/// leaves its calls behind as unlabeled rows.
#[tokio::test]
async fn deleting_something_with_calls_is_refused_and_says_how_many() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1_000))
        .await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(600_000))
        .await;
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    let system = systems["results"][0]["id"].as_i64().expect("an id");
    let (_, talkgroups) = app.admin_get("/api/admin/talkgroups").await;
    let talkgroup = talkgroups["results"][0]["id"].as_i64().expect("an id");

    for (path, reason) in [
        (format!("/api/admin/systems/{system}"), "system-has-calls"),
        (
            format!("/api/admin/talkgroups/{talkgroup}"),
            "talkgroup-has-calls",
        ),
    ] {
        let (status, refused) = app.admin_delete(&path).await;

        assert_eq!(status, 409, "{path}");
        assert_eq!(slug(&refused), reason, "{refused}");
        // The count is in the body, because "some" is not a number an Operator
        // can decide anything with.
        assert_eq!(refused["calls"], 2, "{refused}");
    }
    // ...and nothing went.
    assert_eq!(app.count::<call::Entity>().await, 2);
    assert_eq!(app.count::<system::Entity>().await, 1);
}

/// ...and `?force=true` is the Operator saying it out loud: the Calls go, and so
/// do their audio objects, through retention's own pass rather than a second
/// copy of it that could strand them in a bucket.
#[tokio::test]
async fn a_forced_delete_takes_the_calls_and_their_audio() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100)).await;
    app.upload_ok(CallUpload::new().talkgroup(200)).await;
    assert_eq!(app.object_keys().await.len(), 2);
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    let system = systems["results"][0]["id"].as_i64().expect("an id");

    let (status, _) = app
        .admin_delete(&format!("/api/admin/systems/{system}?force=true"))
        .await;

    assert_eq!(status, 204);
    assert_eq!(app.count::<call::Entity>().await, 0);
    assert_eq!(app.count::<system::Entity>().await, 0);
    assert_eq!(
        app.count::<talkgroup::Entity>().await,
        0,
        "and its channels"
    );
    assert!(
        app.object_keys().await.is_empty(),
        "audio must not be left orphaned in the store"
    );
}

/// Forcing one Talkgroup takes its Calls and leaves its neighbours' alone — the
/// blast radius is exactly what was named.
#[tokio::test]
async fn a_forced_talkgroup_delete_spares_the_rest_of_the_system() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100)).await;
    app.upload_ok(CallUpload::new().talkgroup(200)).await;
    let (_, talkgroups) = app.admin_get("/api/admin/talkgroups").await;
    let doomed = talkgroups["results"][0]["id"].as_i64().expect("an id");

    let (status, _) = app
        .admin_delete(&format!("/api/admin/talkgroups/{doomed}?force=true"))
        .await;

    assert_eq!(status, 204);
    assert_eq!(app.count::<call::Entity>().await, 1);
    assert_eq!(app.count::<talkgroup::Entity>().await, 1);
    assert_eq!(app.object_keys().await.len(), 1);
}

/// Naming an apparatus, and then un-naming it.
///
/// A Unit delete needs no force and refuses nothing: a Call names the radios it
/// heard by **Ref** (`call_units`), never by a Unit's Id, so deleting the roster
/// entry takes no Call with it — the archive goes back to showing the bare
/// number, exactly as it did before anybody curated one (#47).
#[tokio::test]
async fn naming_a_radio_reaches_the_archive_and_un_naming_it_leaves_the_calls() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    // A radio that said nothing about itself, so what the archive shows is
    // whatever an Operator wrote down and nothing else.
    app.upload_ok(CallUpload::new().talkgroup(100).set("source", 4242))
        .await;
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    let system_id = systems["results"][0]["id"].as_i64().expect("an id");

    let (status, created) = app
        .admin_post(
            "/api/admin/units",
            json!({"systemId": system_id, "ref": 4242, "label": "Engine 1"}),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let unit_id = created["id"].as_i64().expect("an id");

    let page = app.get_json("/api/calls").await;
    assert_eq!(page["results"][0]["unitLabel"], "Engine 1", "{page}");

    let (status, _) = app
        .admin_delete(&format!("/api/admin/units/{unit_id}"))
        .await;

    assert_eq!(status, 204);
    assert_eq!(app.count::<call::Entity>().await, 1);
    assert_eq!(app.count::<unit::Entity>().await, 0);
    let page = app.get_json("/api/calls").await;
    assert_eq!(page["results"][0]["unitRef"], 4242, "{page}");
    assert!(page["results"][0]["unitLabel"].is_null(), "{page}");
}

// ---------------------------------------------------------------------------
// Talkgroups
// ---------------------------------------------------------------------------

/// A curated Talkgroup, and the proof that curation reaches the surfaces a
/// Listener actually sees: the catalog the selection panel is built from.
#[tokio::test]
async fn curating_a_talkgroup_reaches_the_catalog() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11, "label": "Fulton"}))
        .await;
    let system_id = system["id"].as_i64().expect("an id");

    let (status, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({
                "systemId": system_id,
                "ref": 100,
                "label": "FD1",
                "name": "Fire Dispatch",
                "tag": "Fire",
                "groups": ["Fire", "Dispatch"],
                "led": "RED",
            }),
        )
        .await;

    assert_eq!(status, 201, "{created}");
    assert_eq!(created["label"], "FD1");
    assert_eq!(created["tag"], "Fire");
    // Sorted, so the panel and the row agree about the order.
    assert_eq!(created["groups"], json!(["Dispatch", "Fire"]));
    // The palette is case-insensitive on a form, as it is in a CSV.
    assert_eq!(created["led"], "red");
    assert_eq!(created["systemRef"], 11);
    assert_eq!(created["systemLabel"], "Fulton");

    let catalog = app.get_json("/api/catalog").await;
    let channel = &catalog["systems"][0]["talkgroups"][0];
    assert_eq!(channel["label"], "FD1", "{catalog}");
    assert_eq!(channel["tag"], "Fire", "{catalog}");
    assert_eq!(channel["led"], "red", "{catalog}");
    assert_eq!(channel["groups"], json!(["Dispatch", "Fire"]));
}

/// A patch changes only what it names — and `groups` **replaces** the set, which
/// is the CSV importer's rule (#18): a set-valued field with no way to spell
/// "none" could add a Group and never take one away.
#[tokio::test]
async fn a_talkgroup_patch_changes_only_what_it_names() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let (_, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({
                "systemId": system["id"],
                "ref": 100,
                "label": "FD1",
                "tag": "Fire",
                "groups": ["Fire"],
                "led": "red",
            }),
        )
        .await;
    let id = created["id"].as_i64().expect("an id");

    let (status, edited) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{id}"),
            json!({"name": "Fire Dispatch"}),
        )
        .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["name"], "Fire Dispatch");
    assert_eq!(edited["label"], "FD1", "untouched");
    assert_eq!(edited["tag"], "Fire", "untouched");
    assert_eq!(edited["led"], "red", "untouched");

    let (_, cleared) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{id}"),
            json!({"tag": null, "led": null, "groups": []}),
        )
        .await;
    assert!(cleared["tag"].is_null(), "{cleared}");
    assert!(cleared["led"].is_null(), "{cleared}");
    assert_eq!(cleared["groups"], json!([]));
    assert_eq!(cleared["label"], "FD1", "still untouched");
}

/// An LED outside the palette is refused by name, with the palette in the
/// message — a typo must never become a channel that renders with no LED.
#[tokio::test]
async fn an_led_outside_the_palette_is_refused() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;

    let (status, refused) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system["id"], "ref": 100, "led": "puce"}),
        )
        .await;

    assert_eq!(status, 400);
    assert_eq!(slug(&refused), "unknown-led");
    let detail = refused["detail"].as_str().expect("a detail");
    assert!(detail.contains("puce"), "{detail}");
    assert!(detail.contains("red"), "{detail}");
    assert_eq!(app.count::<talkgroup::Entity>().await, 0);
}

/// A body naming a System that is not there is a **400**, not a 404: the route
/// and its row are fine, and a 404 here reads as "no such endpoint".
#[tokio::test]
async fn a_talkgroup_under_no_system_is_refused() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, refused) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": 999, "ref": 100}),
        )
        .await;

    assert_eq!(status, 400);
    assert_eq!(slug(&refused), "no-such-system");
}

/// A Ref another channel already answers to is refused — its own, **or** one it
/// holds as a member Ref (#45), because both resolve to a channel at ingest and
/// two owners would be a Call that could land on either.
#[tokio::test]
async fn a_talkgroup_ref_cannot_be_taken_from_a_channel_or_its_members() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let system_id = system["id"].as_i64().expect("an id");
    app.admin_post(
        "/api/admin/talkgroups",
        json!({"systemId": system_id, "ref": 100}),
    )
    .await;
    // A member Ref folded into that channel, the way #45's merge leaves one.
    app.seed_member_ref(11, 100, 101).await;

    for taken in [100, 101] {
        let (status, refused) = app
            .admin_post(
                "/api/admin/talkgroups",
                json!({"systemId": system_id, "ref": taken}),
            )
            .await;

        assert_eq!(status, 409, "ref {taken}");
        assert_eq!(slug(&refused), "talkgroup-ref-taken", "{refused}");
    }
}

/// **Spec US 46.** Eighty rows, one action, one transaction — where rdio needs a
/// `PUT` of the whole configuration document with eighty rows changed inside it.
#[tokio::test]
async fn one_bulk_action_assigns_groups_and_a_tag_across_many_rows() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let mut ids = Vec::new();
    for r#ref in 100..105 {
        let (_, created) = app
            .admin_post(
                "/api/admin/talkgroups",
                json!({"systemId": system["id"], "ref": r#ref, "groups": ["Old"]}),
            )
            .await;
        ids.push(created["id"].as_i64().expect("an id"));
    }

    let (status, assigned) = app
        .admin_post(
            "/api/admin/talkgroups/assign",
            json!({
                "ids": ids,
                "addGroups": ["Fire", "Dispatch"],
                "removeGroups": ["Old"],
                "tag": "Fire Dispatch",
            }),
        )
        .await;

    assert_eq!(status, 200, "{assigned}");
    assert_eq!(assigned["changed"], 5);
    let (_, listed) = app.admin_get("/api/admin/talkgroups").await;
    for row in listed["results"].as_array().expect("rows") {
        assert_eq!(row["groups"], json!(["Dispatch", "Fire"]), "{row}");
        assert_eq!(row["tag"], "Fire Dispatch", "{row}");
    }
    // Groups and Tags named in a bulk action are created, as the CSV importer
    // creates them — an Operator typing "Fire" means the Group.
    let (_, groups) = app.admin_get("/api/admin/groups").await;
    assert!(names(&groups).contains(&"Dispatch".to_string()), "{groups}");
}

/// A bulk action reports what it really reached, so a stale selection is visible
/// rather than swallowed — and a Group nobody has is nothing to remove, not an
/// error, because a mixed selection is exactly where that happens.
#[tokio::test]
async fn a_bulk_action_reports_the_rows_it_reached() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let (_, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system["id"], "ref": 100}),
        )
        .await;
    let id = created["id"].as_i64().expect("an id");

    let (status, assigned) = app
        .admin_post(
            "/api/admin/talkgroups/assign",
            json!({"ids": [id, 998, 999], "removeGroups": ["Nothing"]}),
        )
        .await;

    assert_eq!(status, 200);
    assert_eq!(assigned["changed"], 1, "two of the three named no row");
}

/// **The blacklist toggle, proved against ingest.** A Talkgroup an Operator
/// blacklists in the browser stops being ingested — and the Ref lands on the
/// System's own list, which is where `repo::is_blacklisted` reads it.
#[tokio::test]
async fn blacklisting_a_talkgroup_stops_its_calls_being_ingested() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1_000))
        .await;
    let (_, talkgroups) = app.admin_get("/api/admin/talkgroups").await;
    let id = talkgroups["results"][0]["id"].as_i64().expect("an id");
    assert_eq!(talkgroups["results"][0]["blacklisted"], false);

    let (status, blacklisted) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{id}"),
            json!({"blacklisted": true}),
        )
        .await;
    assert_eq!(status, 200, "{blacklisted}");
    assert_eq!(blacklisted["blacklisted"], true);

    // The next Call for that channel is dropped — answered `200` so the recorder
    // never retries, which is why the count is the assertion (#8).
    app.upload_ok(CallUpload::new().talkgroup(100).at(600_000))
        .await;
    assert_eq!(app.count::<call::Entity>().await, 1);
    // ...and the System's own list is where it landed, readable from that form.
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    assert_eq!(
        systems["results"][0]["blacklist"],
        json!([100]),
        "{systems}"
    );

    // Off again, and ingest resumes.
    app.admin_patch(
        &format!("/api/admin/talkgroups/{id}"),
        json!({"blacklisted": false}),
    )
    .await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1_200_000))
        .await;
    assert_eq!(app.count::<call::Entity>().await, 2);
}

/// The other half of the same column: the System form's own list, for a Ref no
/// channel exists for yet — a patch-minted TGID an Operator wants refused
/// **before** its first sighting, which the per-row toggle cannot express.
#[tokio::test]
async fn a_system_blacklist_refuses_a_ref_nothing_has_heard_yet() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1_000))
        .await;
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    let id = systems["results"][0]["id"].as_i64().expect("an id");

    let (status, edited) = app
        .admin_patch(
            &format!("/api/admin/systems/{id}"),
            json!({"blacklist": [999, 500, 500]}),
        )
        .await;

    assert_eq!(status, 200, "{edited}");
    // Canonical: sorted and de-duplicated, so the form shows back one policy.
    assert_eq!(edited["blacklist"], json!([500, 999]));
    app.upload_ok(CallUpload::new().talkgroup(999).at(600_000))
        .await;
    assert_eq!(app.count::<call::Entity>().await, 1, "999 was refused");
    // ...and the channel that was never blacklisted still ingests.
    app.upload_ok(CallUpload::new().talkgroup(100).at(1_200_000))
        .await;
    assert_eq!(app.count::<call::Entity>().await, 2);
}

/// Renumbering a blacklisted channel takes its blacklisting with it. The list
/// holds Refs, so leaving the old one behind would refuse a Ref nobody owns and
/// let the new one through — the Operator's policy silently inverted by an edit
/// that never mentioned it.
#[tokio::test]
async fn a_renumbered_channel_keeps_its_blacklisting() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100).at(1_000))
        .await;
    let (_, talkgroups) = app.admin_get("/api/admin/talkgroups").await;
    let id = talkgroups["results"][0]["id"].as_i64().expect("an id");
    app.admin_patch(
        &format!("/api/admin/talkgroups/{id}"),
        json!({"blacklisted": true}),
    )
    .await;

    let (status, moved) = app
        .admin_patch(&format!("/api/admin/talkgroups/{id}"), json!({"ref": 101}))
        .await;

    assert_eq!(status, 200, "{moved}");
    assert_eq!(moved["ref"], 101);
    assert_eq!(
        moved["blacklisted"], true,
        "the policy followed the channel"
    );
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    assert_eq!(
        systems["results"][0]["blacklist"],
        json!([101]),
        "{systems}"
    );
}

/// Deleting a channel takes its blacklisting off the System, or the entry would
/// go on refusing a Ref that names nothing — and would silently reappear as a
/// blacklisted channel the moment auto-populate recreated it.
#[tokio::test]
async fn deleting_a_channel_takes_its_blacklisting_with_it() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let (_, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system["id"], "ref": 100, "blacklisted": true}),
        )
        .await;
    assert_eq!(created["blacklisted"], true);

    app.admin_delete(&format!(
        "/api/admin/talkgroups/{}",
        created["id"].as_i64().expect("an id")
    ))
    .await;

    let (_, systems) = app.admin_get("/api/admin/systems").await;
    assert_eq!(systems["results"][0]["blacklist"], json!([]), "{systems}");
}

/// The Talkgroup listing filters and pages, because a county is hundreds of
/// channels and a form that renders all of them is one nobody can use on a
/// phone. Every filter is asserted, and the **total describes the rows above
/// it** — the rule #98 pinned for the archive.
#[tokio::test]
async fn the_talkgroup_listing_filters_and_pages() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, fulton) = app
        .admin_post("/api/admin/systems", json!({"ref": 11, "label": "Fulton"}))
        .await;
    let (_, other) = app
        .admin_post("/api/admin/systems", json!({"ref": 12, "label": "Other"}))
        .await;
    for (system, r#ref, label, tag, group) in [
        (&fulton, 100, "Fire Dispatch", "Fire", "Fire"),
        (&fulton, 101, "Fire Tac 1", "Fire", "Fire"),
        (&fulton, 200, "Police Dispatch", "Law", "Law"),
        (&other, 300, "County EMS", "EMS", "EMS"),
    ] {
        app.admin_post(
            "/api/admin/talkgroups",
            json!({
                "systemId": system["id"],
                "ref": r#ref,
                "label": label,
                "tag": tag,
                "groups": [group],
            }),
        )
        .await;
    }

    for (query, expected) in [
        ("", 4),
        // A System **Ref**, which is what an Operator reads on the screen.
        ("?system=11", 3),
        ("?system=12", 1),
        // Free text over the label, case-insensitively on both dialects.
        ("?q=fire", 2),
        ("?q=FIRE", 2),
        ("?q=dispatch", 2),
        // ...and over the Ref, because a number an Operator types is usually one.
        ("?q=200", 1),
        ("?group=Fire", 2),
        ("?tag=Law", 1),
        // Filters compose.
        ("?system=11&tag=Fire", 2),
        ("?system=12&tag=Fire", 0),
        // A value nothing has is an empty page, never everything.
        ("?group=Nothing", 0),
        ("?system=99", 0),
    ] {
        let (status, page) = app
            .admin_get(&format!("/api/admin/talkgroups{query}"))
            .await;

        assert_eq!(status, 200, "{query}");
        assert_eq!(page["count"], expected, "{query} -> {page}");
        assert_eq!(
            page["results"].as_array().expect("rows").len() as u64,
            expected,
            "{query}"
        );
    }

    // The window is applied to the rows and nowhere else, so the total keeps
    // describing the whole match while the page walks it.
    let (_, first) = app.admin_get("/api/admin/talkgroups?limit=3").await;
    assert_eq!(first["count"], 4);
    assert_eq!(first["results"].as_array().expect("rows").len(), 3);
    assert_eq!(first["hasMore"], true);
    let (_, last) = app
        .admin_get("/api/admin/talkgroups?limit=3&offset=3")
        .await;
    assert_eq!(last["count"], 4);
    assert_eq!(last["results"].as_array().expect("rows").len(), 1);
    assert_eq!(last["hasMore"], false);
}

/// The blacklist filter is not a column — it is a Ref's membership of a list on
/// the parent — so it is applied in Rust, and this is what proves the total
/// still describes the rows.
#[tokio::test]
async fn the_talkgroup_listing_filters_on_the_blacklist() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    for (r#ref, blacklisted) in [(100, true), (101, false), (102, true)] {
        app.admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system["id"], "ref": r#ref, "blacklisted": blacklisted}),
        )
        .await;
    }

    for (query, expected) in [
        ("?blacklisted=true", 2),
        ("?blacklisted=false", 1),
        ("?blacklisted=1", 2),
        ("", 3),
    ] {
        let (_, page) = app
            .admin_get(&format!("/api/admin/talkgroups{query}"))
            .await;

        assert_eq!(page["count"], expected, "{query} -> {page}");
        assert_eq!(
            page["results"].as_array().expect("rows").len() as u64,
            expected,
            "{query}"
        );
    }
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

/// The Unit listing filters the way an Operator sitting down to name a fleet
/// works: by System, by text, and by **what nobody has named yet** — which no
/// text search can reach.
#[tokio::test]
async fn the_unit_listing_filters_by_system_text_and_namelessness() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, fulton) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let (_, other) = app
        .admin_post("/api/admin/systems", json!({"ref": 12}))
        .await;
    for (system, r#ref, label) in [
        (&fulton, 1201, Some("Engine 1")),
        (&fulton, 1202, Some("Engine 2")),
        (&fulton, 1203, None),
        (&other, 4471, None),
    ] {
        app.admin_post(
            "/api/admin/units",
            json!({"systemId": system["id"], "ref": r#ref, "label": label}),
        )
        .await;
    }

    for (query, expected) in [
        ("", 4),
        ("?system=11", 3),
        ("?q=engine", 2),
        ("?q=ENGINE", 2),
        ("?q=1202", 1),
        ("?unnamed=true", 2),
        ("?unnamed=false", 2),
        ("?system=11&unnamed=true", 1),
        ("?limit=2", 4),
    ] {
        let (status, page) = app.admin_get(&format!("/api/admin/units{query}")).await;

        assert_eq!(status, 200, "{query}");
        assert_eq!(page["count"], expected, "{query} -> {page}");
    }
}

/// A radio id another apparatus already answers to is refused — its own Ref, or
/// one inside a **Range** it owns (#45), because a Ref owned twice is a Call
/// whose radio could resolve to either.
#[tokio::test]
async fn a_unit_ref_cannot_be_taken_from_a_unit_or_its_ranges() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let system_id = system["id"].as_i64().expect("an id");
    app.admin_post(
        "/api/admin/units",
        json!({"systemId": system_id, "ref": 1201, "label": "Engine 1"}),
    )
    .await;
    // A Range that apparatus owns, the way a unit CSV writes one (#47).
    app.seed_unit_range(11, 1201, 1300, 1399).await;

    for taken in [1201, 1350] {
        let (status, refused) = app
            .admin_post(
                "/api/admin/units",
                json!({"systemId": system_id, "ref": taken}),
            )
            .await;

        assert_eq!(status, 409, "ref {taken}");
        assert_eq!(slug(&refused), "unit-ref-taken", "{refused}");
    }
    // ...and a radio id nobody owns is free.
    let (status, _) = app
        .admin_post(
            "/api/admin/units",
            json!({"systemId": system_id, "ref": 4471}),
        )
        .await;
    assert_eq!(status, 201);
}

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

/// **A key is shown exactly once**, and never again — the listing carries no
/// secret and no hash, because ours are stored hashed where rdio stores them in
/// plaintext for anyone who reaches its admin page or a backup of its database.
#[tokio::test]
async fn a_key_is_shown_once_and_then_never_again() {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, issued) = app
        .admin_post(
            "/api/admin/api-keys",
            json!({"label": "the pi", "systemRef": 11}),
        )
        .await;

    assert_eq!(status, 201, "{issued}");
    let secret = issued["key"].as_str().expect("a key").to_string();
    assert!(!secret.is_empty());
    assert_eq!(issued["label"], "the pi");
    assert_eq!(issued["systemRef"], 11);
    assert_eq!(issued["disabled"], false);

    // ...and it is a key that actually works, scoped to the System it names.
    app.upload_ok(CallUpload::new().key(&secret).system(11))
        .await;

    let (_, listed) = app.admin_get("/api/admin/api-keys").await;
    let listing = listed.to_string();
    assert!(
        !listing.contains(&secret),
        "the secret must not be listable"
    );
    assert!(!listing.contains("keyHash"), "{listing}");
    assert!(listing.contains("the pi"), "{listing}");
}

/// A key scoped to one System does not open another — the whole point of the
/// scope, and the reason a Ref rather than an Id: an Operator can cut a recorder
/// its key before the System exists.
#[tokio::test]
async fn a_scoped_key_opens_only_its_own_system() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, issued) = app
        .admin_post("/api/admin/api-keys", json!({"systemRef": 11}))
        .await;
    let secret = issued["key"].as_str().expect("a key").to_string();

    app.upload_ok(CallUpload::new().key(&secret).system(11))
        .await;
    let (status, body) = app.upload(CallUpload::new().key(&secret).system(12)).await;

    assert_eq!(status, 401, "{body}");
    assert_eq!(app.count::<call::Entity>().await, 1);
}

/// Disable is the durable off, and re-enable brings it back — one row, one flag,
/// and a recorder that starts being refused on its next upload.
#[tokio::test]
async fn a_disabled_key_is_refused_until_it_is_enabled_again() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, issued) = app.admin_post("/api/admin/api-keys", json!({})).await;
    let secret = issued["key"].as_str().expect("a key").to_string();
    let id = issued["id"].as_i64().expect("an id");

    let (status, disabled) = app
        .admin_patch(
            &format!("/api/admin/api-keys/{id}"),
            json!({"disabled": true}),
        )
        .await;
    assert_eq!(status, 200, "{disabled}");
    assert_eq!(disabled["disabled"], true);

    let (status, _) = app.upload(CallUpload::new().key(&secret)).await;
    assert_eq!(status, 401);

    app.admin_patch(
        &format!("/api/admin/api-keys/{id}"),
        json!({"disabled": false}),
    )
    .await;
    app.upload_ok(CallUpload::new().key(&secret)).await;
    assert_eq!(app.count::<call::Entity>().await, 1);
}

/// Revoking one deletes the row, and the recorder holding it is refused.
#[tokio::test]
async fn a_revoked_key_stops_working() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, issued) = app.admin_post("/api/admin/api-keys", json!({})).await;
    let secret = issued["key"].as_str().expect("a key").to_string();

    let (status, _) = app
        .admin_delete(&format!(
            "/api/admin/api-keys/{}",
            issued["id"].as_i64().expect("an id")
        ))
        .await;

    assert_eq!(status, 204);
    let (status, _) = app.upload(CallUpload::new().key(&secret)).await;
    assert_eq!(status, 401);
}

/// A key can be relabelled and re-scoped, and `null` is how a scope goes back to
/// every System — which an absent field could never say.
#[tokio::test]
async fn a_key_can_be_relabelled_and_rescoped() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, issued) = app
        .admin_post("/api/admin/api-keys", json!({"systemRef": 11}))
        .await;
    let secret = issued["key"].as_str().expect("a key").to_string();
    let id = issued["id"].as_i64().expect("an id");

    let (_, edited) = app
        .admin_patch(
            &format!("/api/admin/api-keys/{id}"),
            json!({"label": "the shed", "systemRef": null}),
        )
        .await;

    assert_eq!(edited["label"], "the shed");
    assert!(edited["systemRef"].is_null(), "{edited}");
    // ...and an unscoped key opens the System it could not before.
    app.upload_ok(CallUpload::new().key(&secret).system(12))
        .await;
}

// ---------------------------------------------------------------------------
// The edits each patch arm makes
// ---------------------------------------------------------------------------

/// Every field a Talkgroup patch can *set*, as opposed to clear — including the
/// Tag, which is resolved-or-created the way the CSV importer creates one (#18),
/// and the nullable enhancement scope (#20) that nothing else can reach.
#[tokio::test]
async fn a_talkgroup_patch_sets_every_field_it_names() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let (_, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system["id"], "ref": 100}),
        )
        .await;
    let id = created["id"].as_i64().expect("an id");

    let (status, edited) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{id}"),
            json!({
                "label": "FD1",
                "tag": "  Fire  ",
                "led": "cyan",
                "enhancement": true,
            }),
        )
        .await;

    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["label"], "FD1");
    // Trimmed, so " Fire " and "Fire" are never two Tags.
    assert_eq!(edited["tag"], "Fire");
    assert_eq!(edited["led"], "cyan");
    assert_eq!(edited["enhancement"], true);
    // The Tag was created by naming it, as an import creates one.
    let (_, tags) = app.admin_get("/api/admin/tags").await;
    assert_eq!(names(&tags), vec!["Fire"]);

    // ...and a Tag named as whitespace is no Tag at all, not a Tag called "".
    let (_, blanked) = app
        .admin_patch(
            &format!("/api/admin/talkgroups/{id}"),
            json!({"tag": "   "}),
        )
        .await;
    assert!(blanked["tag"].is_null(), "{blanked}");
}

/// The per-System auto-populate flag (#8) — a column nothing else in
/// Radio-Scout can set, and the reason a System keeps discovering channels while
/// the instance-wide switch is off.
#[tokio::test]
async fn a_system_patch_sets_auto_populate() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, created) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let id = created["id"].as_i64().expect("an id");
    assert_eq!(created["autoPopulate"], false);

    let (status, edited) = app
        .admin_patch(
            &format!("/api/admin/systems/{id}"),
            json!({"autoPopulate": true}),
        )
        .await;

    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["autoPopulate"], true);
}

/// Naming and renumbering an apparatus, and the collision a renumber can hit.
#[tokio::test]
async fn a_unit_is_renamed_and_renumbered() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let system_id = system["id"].as_i64().expect("an id");
    let (_, created) = app
        .admin_post(
            "/api/admin/units",
            json!({"systemId": system_id, "ref": 1201}),
        )
        .await;
    let id = created["id"].as_i64().expect("an id");
    app.admin_post(
        "/api/admin/units",
        json!({"systemId": system_id, "ref": 1202}),
    )
    .await;

    let (status, named) = app
        .admin_patch(
            &format!("/api/admin/units/{id}"),
            json!({"label": "Engine 1"}),
        )
        .await;
    assert_eq!(status, 200, "{named}");
    assert_eq!(named["label"], "Engine 1");
    assert_eq!(
        named["ref"], 1201,
        "the ref was not named, so it did not move"
    );

    // ...and renumbering alone keeps the name, because a patch changes only
    // what it names.
    let (status, named) = app
        .admin_patch(&format!("/api/admin/units/{id}"), json!({"ref": 1301}))
        .await;
    assert_eq!(status, 200, "{named}");
    assert_eq!(named["ref"], 1301);
    assert_eq!(named["label"], "Engine 1");

    // A radio id its neighbour already answers to is refused...
    let (status, refused) = app
        .admin_patch(&format!("/api/admin/units/{id}"), json!({"ref": 1202}))
        .await;
    assert_eq!(status, 409);
    assert_eq!(slug(&refused), "unit-ref-taken");

    // ...its own is not, and `null` un-names it.
    let (status, kept) = app
        .admin_patch(
            &format!("/api/admin/units/{id}"),
            json!({"ref": 1301, "label": null}),
        )
        .await;
    assert_eq!(status, 200, "{kept}");
    assert!(kept["label"].is_null(), "{kept}");

    let (status, refused) = app
        .admin_patch("/api/admin/units/999", json!({"label": "Ghost"}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(slug(&refused), "unit-not-found");
}

/// A bulk action can clear the Tag of every row it names, and one naming no
/// rows at all changes nothing rather than failing.
#[tokio::test]
async fn a_bulk_action_clears_tags_and_tolerates_an_empty_selection() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    let (_, created) = app
        .admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system["id"], "ref": 100, "tag": "Fire"}),
        )
        .await;
    let id = created["id"].as_i64().expect("an id");

    let (status, assigned) = app
        .admin_post(
            "/api/admin/talkgroups/assign",
            json!({"ids": [id], "tag": null}),
        )
        .await;
    assert_eq!(status, 200, "{assigned}");
    assert_eq!(assigned["changed"], 1);
    let (_, listed) = app.admin_get("/api/admin/talkgroups").await;
    assert!(listed["results"][0]["tag"].is_null(), "{listed}");

    // An empty selection is a no-op, not an error — a form submitted with
    // nothing ticked must not be a 500.
    let (status, nothing) = app
        .admin_post(
            "/api/admin/talkgroups/assign",
            json!({"ids": [], "addGroups": ["Fire"]}),
        )
        .await;
    assert_eq!(status, 200, "{nothing}");
    assert_eq!(nothing["changed"], 0);
}

// ---------------------------------------------------------------------------
// When the database refuses
// ---------------------------------------------------------------------------

/// A curation request whose database refuses it is a **5xx that says where it
/// broke** — never a leaked SQL error, and never a silent success.
///
/// Reached through the statement seam (#97) rather than by damaging a schema:
/// the handle every statement goes through is told to refuse everything naming
/// one table, which is how an arm that only a broken database reaches becomes a
/// thing a test can drive through the app's own front door.
#[rstest]
#[case::listing_groups(reqwest::Method::GET, "/api/admin/groups", "groups", None)]
#[case::listing_systems(reqwest::Method::GET, "/api/admin/systems", "systems", None)]
#[case::listing_keys(reqwest::Method::GET, "/api/admin/api-keys", "api_keys", None)]
#[case::creating_a_group(
    reqwest::Method::POST,
    "/api/admin/groups",
    "groups",
    Some(json!({"name": "Fire"}))
)]
#[case::creating_a_key(
    reqwest::Method::POST,
    "/api/admin/api-keys",
    "api_keys",
    Some(json!({}))
)]
#[case::listing_talkgroups(reqwest::Method::GET, "/api/admin/talkgroups", "talkgroups", None)]
#[case::listing_units(reqwest::Method::GET, "/api/admin/units", "units", None)]
#[tokio::test]
async fn a_refused_statement_becomes_a_named_failure(
    #[case] method: reqwest::Method,
    #[case] path: &str,
    #[case] table: &str,
    #[case] body: Option<Value>,
) {
    let app = TestApp::spawn().await;
    app.login().await;
    app.refuse_statements_on(table);

    let (status, told) = app.admin(method, path, body).await;

    assert_eq!(status, 500, "{told}");
    // The caller gets an id and nothing else — no SQL, no table name (#29).
    let told = told.as_str().unwrap_or_default();
    assert!(told.contains("request id"), "{told}");
    assert!(!told.contains(table), "the cause must not travel: {told}");
}

// ---------------------------------------------------------------------------
// What a page costs
// ---------------------------------------------------------------------------

/// **A page costs the same whatever size it is.** The listing behind the county
/// screen has to page in the *database*, or `limit` bounds only what gets
/// serialized and a Pi still materializes every channel an Operator owns.
///
/// Asserted the way `tests/archive.rs` and `tests/live.rs` assert it (#86): two
/// sizes over the same rows, equal statement counts — never a pinned number,
/// which would break on any unrelated query added beside it.
#[rstest]
#[case::talkgroups("/api/admin/talkgroups")]
#[case::units("/api/admin/units")]
#[tokio::test]
async fn a_page_costs_the_same_at_any_size(#[case] path: &str) {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    for n in 0..12 {
        app.admin_post(
            "/api/admin/talkgroups",
            json!({"systemId": system["id"], "ref": 100 + n, "tag": "Fire", "groups": ["Fire"]}),
        )
        .await;
        app.admin_post(
            "/api/admin/units",
            json!({"systemId": system["id"], "ref": 1200 + n, "label": format!("Engine {n}")}),
        )
        .await;
    }

    let before = app.statements_issued();
    let (_, small) = app.admin_get(&format!("{path}?limit=2")).await;
    let two = app.statements_issued() - before;

    let before = app.statements_issued();
    let (_, big) = app.admin_get(&format!("{path}?limit=12")).await;
    let twelve = app.statements_issued() - before;

    assert_eq!(small["results"].as_array().expect("rows").len(), 2);
    assert_eq!(big["results"].as_array().expect("rows").len(), 12);
    // ...and both report the same total, because the window is applied to the
    // rows and nowhere else.
    assert_eq!(small["count"], 12);
    assert_eq!(big["count"], 12);
    assert_eq!(
        two, twelve,
        "a page of 12 cost {twelve} statements where a page of 2 cost {two}"
    );
}

/// ...and a bulk action costs the same whatever the selection, which is the
/// difference between "one action" and eighty round trips wearing its clothes.
#[tokio::test]
async fn a_bulk_action_costs_the_same_however_many_rows_it_names() {
    let app = TestApp::spawn().await;
    app.login().await;
    let (_, system) = app
        .admin_post("/api/admin/systems", json!({"ref": 11}))
        .await;
    // Both actions steady-state: the *first* use of a name creates the Group
    // and the Tag, which costs the same two statements however many rows are
    // selected — so measuring it would be measuring creation, not the loop
    // this test exists to have deleted.
    app.admin_post("/api/admin/groups", json!({"name": "Fire"}))
        .await;
    app.admin_post("/api/admin/tags", json!({"name": "Dispatch"}))
        .await;
    let mut ids = Vec::new();
    for n in 0..16 {
        let (_, created) = app
            .admin_post(
                "/api/admin/talkgroups",
                json!({"systemId": system["id"], "ref": 100 + n}),
            )
            .await;
        ids.push(created["id"].as_i64().expect("an id"));
    }

    let before = app.statements_issued();
    app.admin_post(
        "/api/admin/talkgroups/assign",
        json!({"ids": ids[..2], "addGroups": ["Fire"], "tag": "Dispatch"}),
    )
    .await;
    let two = app.statements_issued() - before;

    let before = app.statements_issued();
    app.admin_post(
        "/api/admin/talkgroups/assign",
        json!({"ids": ids[2..], "addGroups": ["Fire"], "tag": "Dispatch"}),
    )
    .await;
    let fourteen = app.statements_issued() - before;

    assert_eq!(
        two, fourteen,
        "assigning 14 rows cost {fourteen} statements where 2 cost {two}"
    );
    // ...and it really did assign them.
    let (_, listed) = app.admin_get("/api/admin/talkgroups?group=Fire").await;
    assert_eq!(listed["count"], 16, "{listed}");
}

/// A force-delete that fails *after* the Calls are gone leaves the entity
/// standing, so the Operator can ask again — the recoverable direction.
///
/// The other order would leave Calls pointing at a System that is not there,
/// which nothing could resolve and no retry could fix. This pins which way
/// round it is.
#[tokio::test]
async fn a_forced_delete_that_breaks_leaves_the_entity_to_retry() {
    let app = TestApp::with_key("k").await;
    app.login().await;
    app.upload_ok(CallUpload::new().talkgroup(100)).await;
    let (_, systems) = app.admin_get("/api/admin/systems").await;
    let id = systems["results"][0]["id"].as_i64().expect("an id");
    // The Calls purge before the entity's own transaction opens, so refusing
    // the `systems` write is refusing the second half.
    app.refuse_updates_to("systems");
    app.refuse_statements_on("sites");

    let (status, told) = app
        .admin_delete(&format!("/api/admin/systems/{id}?force=true"))
        .await;

    assert_eq!(status, 500, "{told}");
    assert_eq!(app.count::<call::Entity>().await, 0, "the calls did go");
    assert_eq!(
        app.count::<system::Entity>().await,
        1,
        "and the system is still there to delete again"
    );
}

// ---------------------------------------------------------------------------
// Merge curation (#50, spec US 17)
// ---------------------------------------------------------------------------
//
// What a fold *does* to the archive is `tests/merge.rs`'s, beside the CSV path
// that does the same thing. What is here is what this surface owes: a row that
// is not there, a body it will not read, and the line an Operator finds
// afterwards.

/// A merge route naming a row that is not there is a 404, on both halves and in
/// both directions — a merge screen opened from a stale list must say so rather
/// than answering 500 or, worse, quietly succeeding against nothing.
#[rstest]
#[case::members("/api/admin/talkgroups/999/members", "talkgroup-not-found")]
#[case::ranges("/api/admin/units/999/ranges", "unit-not-found")]
#[tokio::test]
async fn a_merge_route_naming_no_row_is_a_404(#[case] path: &str, #[case] expected: &str) {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, refused) = app.admin_get(path).await;
    assert_eq!(status, 404, "GET {path}: {refused}");
    assert_eq!(slug(&refused), expected);

    let (status, refused) = app.admin_post(path, json!({})).await;
    assert_eq!(status, 404, "POST {path}: {refused}");
    assert_eq!(slug(&refused), expected);
}

/// The delta's fields are closed, like every other body on this surface: a
/// client that misspells `unfold` must be told, not silently have its unmerge
/// dropped on the floor.
#[rstest]
#[case::members("/api/admin/talkgroups/1/members", json!({"unfolded": [8123]}))]
#[case::ranges("/api/admin/units/1/ranges", json!({"added": [{"from": 1, "to": 9}]}))]
#[tokio::test]
async fn a_misspelled_delta_field_is_refused(#[case] path: &str, #[case] body: Value) {
    let app = TestApp::spawn().await;
    app.login().await;

    let (status, _) = app.admin_post(path, body).await;

    assert_eq!(status, 422, "an unknown field is not silently ignored");
}

/// An empty delta is a legitimate request — a form submitted with nothing
/// changed — and answers with an empty report rather than a refusal.
#[tokio::test]
async fn a_delta_that_names_nothing_changes_nothing() {
    let app = TestApp::spawn().await;
    app.login().await;
    app.seed_talkgroup(11, 100).await;
    let id = app
        .talkgroup_by_ref(11, 100)
        .await
        .expect("the talkgroup")
        .id;

    let (status, report) = app
        .admin_post(&format!("/api/admin/talkgroups/{id}/members"), json!({}))
        .await;

    assert_eq!(status, 200, "{report}");
    assert_eq!(report["folded"], 0);
    assert_eq!(report["unfolded"], 0);
    assert_eq!(report["moved"], json!([]));
}
