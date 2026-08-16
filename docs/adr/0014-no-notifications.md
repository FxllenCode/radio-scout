# No notifications — Radio-Scout does not wake a device

## Context

Radio-Scout shipped Web Push in 0.1.0 (#16, spec US 32, ADR-0005 tier 3). It was not a stub. `src/webpush.rs` implemented RFC 8291 message encryption and RFC 8292 VAPID by hand over RustCrypto — deliberately, because the `web-push` crate pulls OpenSSL and would have broken the static-musl and aarch64 build jobs that ADR-0007's one-binary promise rests on — with its tests pinned to both RFCs' worked examples. Around it sat a subscription table, three routes, leading-edge coalescing per device and Talkgroup, the rule that nothing is sent to a listener who is demonstrably listening, a third first-run credential in `.env`, a `[push]` configuration section, a service-worker handler, a Settings switch, and a stub push service in the test harness that decrypted what left the process and asserted on the bytes.

It worked. Nothing about this decision is a retreat from a feature that failed.

#53 was to extend it: an emergency-flagged Call producing an **Alert** — a push notification with an alert-class topic, plus the badge #42 had already shipped. During that ticket's grilling session the maintainer decided, instead, that notifications should not be a feature of this product at all (2026-08-16).

## Decision

**Radio-Scout does not notify.** No Web Push, no device notifications, no permission prompt, no subscription — and no code, configuration, credential or dependency kept against their return.

**"Alert" ceases to exist as a concept.** It was defined as *"a notification fired by something a call's metadata or signal proves… delivered by Web Push and Webhooks"* — a delivery, and delivery is what has been removed. The glossary loses **Alert**, **Push subscription** and **Coalescing**.

What a Recorder proves about a transmission remains, as a **mark on a Call**: the **Emergency** bit (#42, shipped) and, when #55 lands, a tone-out match. A mark is visible, filterable and searchable, and that is now the whole of what it does. This is a strict narrowing of the vocabulary, not a rename — nothing is called an Alert, and no new noun replaces it.

**This is permanent**, on the ADR-0013 precedent and for the same reason: a removal that leaves the door open is one a future contributor or agent session helpfully reverses. It is mirrored as a hard constraint in `CLAUDE.md`. Do not re-propose it, do not scope features that assume it, and do not leave seams "in case it changes."

**Webhooks are not covered by this.** #54 survives, rescoped: an **Operator** configuring a URL to receive their own flagged Calls is arranging their own inbox, which is a different act from this Instance deciding to wake a **Listener** it has never met. It sits beside **Downstream**, not beside notifications.

## Consequences

- **ADR-0005's guaranteed fallback is vacated, and nothing replaces it.** That ADR's escalation ladder for iOS background audio terminated in *Web-Push-only — "the honest floor"*, on the reasoning that a suspended app can at least say something happened. Radio-Scout now **does not bridge a fully-suspended app**: if iOS suspends the tab, the listener hears nothing until they return to it. #33's real-device gate passed 3/3 on the keep-alive mechanism, so the floor has never actually been used — but it was insurance, and the insurance is gone. Should a future iOS release break the keep-alive, the answer is a fresh decision, and the **Station stream** (#66) is the likeliest direction: continuous audio is a better answer for a phone in a pocket than a buzz. That is a direction, not a promise, and nothing may be built as though #66 already covers it.

- **A shipped capability is withdrawn from existing installs.** 0.1.0 Listeners who turned notifications on lose them at 0.2.0. A cached 0.1.0 client degrades quietly by accident rather than by design — it already reads a 404 from `/api/push/key` as "this server has push switched off" and falls silent — but the withdrawal is still real, and is stated in the 0.2.0 notes and in `docs/using.md` rather than left to look like a bug.

- **"What did I miss while I was away" now has one answer, and it is inside the app.** Nothing reaches out. This raises the value of the missed counter and of archive search, which is why #60's counter survived into #78 rather than dying with the ticket that held it.

- **The Instance provisions two credentials, not three.** `RADIO_SCOUT_VAPID_PRIVATE_KEY` is gone, and with it the one credential that had to be *stable* across restarts rather than merely secret — a browser pins the public half when it subscribes, so a regenerated key silently orphaned every existing subscription. That constraint, and the boot-time reasoning it forced, goes.

- **`src/selection.rs` keeps its reason to exist.** It was extracted so one rule could serve three consumers — the live-feed socket, Web Push and the client. Two remain, and the **Downstream** scope (#52) became a third, so the module is not left over-abstracted.

- **The service worker survives, on a different justification.** ADR-0005 recorded that the project owns `sw.ts` via `injectManifest` because *"a generated worker cannot have a `push` handler at all"*. That reason is gone; the worker stays because the update flow needs to message `SKIP_WAITING`, and because it must never answer `/api/*` from cache. The conclusion outlived its argument, which is worth saying out loud rather than leaving a config option looking unmotivated.

- **Four dependencies leave the build** (`p256`, `hkdf`, `aes-gcm`, `base64`), along with ~3,100 lines across both languages and a hand-rolled cryptographic implementation that had to be right. Removing correct, well-tested code is a real loss of work and is accepted deliberately: git holds it, and it was removed in the commit this ADR ships in. Nothing is kept in-tree unused.

- **The coverage ratchet is re-baselined rather than broken.** The frontend floor was raised specifically for #16, and deleting well-covered code can only move the measured number down. ADR-0010 is amended: the ratchet exists to stop *new untested code* eroding the line, and a removal may re-baseline with the before and after recorded. "Never falls" was written about erosion, not about deletion, and a rule quietly ignored once is worth less than a rule amended in the open.
