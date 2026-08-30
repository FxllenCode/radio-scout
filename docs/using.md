# Using Radio-Scout

This is the listener's guide — what the app does and how to get it to do what you want. If you
are the one *running* the instance, see [operating.md](operating.md) and
[deploy.md](deploy.md).

You need no account and no password. Everything about how you listen — what you selected, what
you muted, what you held — lives in your own browser and never leaves it.

The app has four screens, on the tabs at the bottom: **Live**, **Talkgroups**, **Search**,
**Settings**.

---

## Live

<img src="images/live.png" alt="The Live screen" width="620">

Calls play automatically as they arrive, filtered to the Talkgroups you selected. The card
shows what is playing — Talkgroup, System, tag and group, the waveform with playback position,
frequency, TGID, the radio that keyed, and time — with an LED in the Talkgroup's colour.

The **UNIT** line is a name where anybody has given the radio one, and its id otherwise. Either
way it is a link: tap it for that radio's history, which is where "who was that, and where else"
gets answered. **RECENT** carries the same link on every row.

Top right: **Q** is the listening queue, how many Calls are waiting behind this one, and a dot
showing whether the live feed is connected. Tap the **Q** to see what is waiting — see
[The queue](#the-queue) below. Underneath the card is **RECENT**, the handful that just played,
so you can catch what you missed, with **SESSION LOG** beside it for everything else you have
heard since you opened the app.

On a multi-site system, the tag/group line also names the **Site** the Call was heard on, so
simulcast coverage is legible. Single-site systems say nothing there, and neither do recorders
that don't send one.

Badges appear beside a Talkgroup's name when there is something to say about the transmission:

| Badge | What it means |
| --- | --- |
| ⚠ **Emergency** | The radio's emergency button was pressed on this transmission. |
| 📡 **Tone-out** | A station was paged on this Call — your operator wrote down its paging tones, and they were heard in the audio. The badge names it: *Tone-out: Station 12*. |
| 🔒 **Encrypted** | The Talkgroup is encrypted, so there is no audio to hear. |
| 🔗 **Patched** | A dispatcher patched this Talkgroup to others, so the transmission went out on all of them at once. The badge names the channels: *Patched to 54242, 54255*. |

The first three are things *about the transmission*; the last is about how it was carried, and it
is the one rdio-scanner throws away — it routes patched traffic correctly and then never tells you
a Call arrived that way.

None of these notifies anybody. Radio-Scout does not send push notifications and never will —
a badge is something you find, not something that wakes your phone.

Encrypted Calls never play — there is genuinely nothing in them but the vocoder's noise — so
they go straight to **RECENT** rather than into the queue. They are there so a mostly-encrypted
Talkgroup reads as *busy* instead of as a dead feed.

### The controls

| Control | What it does |
| --- | --- |
| **LIVE FEED** | The master switch. See below — it is not the same as Pause. |
| **HOLD SYS** | Play only this System until you release it. Your previous selection comes back when you do. |
| **HOLD TG** | The same, narrowed to just this Talkgroup — for following one incident. |
| **SKIP** | Abandon the current Call and jump to the next in the queue. |
| **REPLAY** | Play the current Call again from the start. |
| **PAUSE** / **RESUME** | Stop and restart playback. Calls keep arriving and queueing while paused. |
| **AVOID** | Mute this Talkgroup so it stops interrupting. An **Undo** appears for a few seconds afterwards. |
| **30 / 60 / 120 MIN** | Avoid this Talkgroup, then bring it back automatically after that long. |
| **PRIORITY** | Put this Talkgroup ahead of the others in the queue. See [Priority](#priority). |
| **AVOIDING _n_** | Appears while anything is muted: the list of what, with one tap to let any of them back in. |

### The queue

Tap the **Q** count and you get the queue itself, in the order it will play:

- **▶ on a row** plays that Call now, instead of waiting for it.
- **✕ on a row** drops it. Nothing else moves, and it is not counted as missed — you looked at
  it and let it go.
- **JUMP TO NEWEST** gives up the whole backlog for the most recent Call there is. The button
  says how many that costs, and the number lands on the *missed* counter under the card — this
  is the one thing you can do here that is counted, because it is the one where you did not read
  what went.

The queue is capped, so a phone that fell a long way behind does not grow an endless backlog.
What the cap gives up is counted as missed too, and it gives up the lowest **Priority** first.

### Priority

Some channels matter more than others when you are behind. Mark a Talkgroup **Priority** — from
the **PRIORITY** control on the Live screen, or the ⚡ at the end of its row under **Talkgroups**
— and its Calls jump the queue instead of waiting their turn.

Three things worth knowing:

- **It applies to what is already waiting.** Mark dispatch while forty Calls are queued and
  those Calls move now; you do not have to wait for the next one.
- **It is queue order, not selection.** A Priority Talkgroup still has to be switched on to be
  heard, and avoiding one still silences it.
- **The cap respects it.** When the queue is full it gives up routine traffic first, so a busy
  night no longer discards the one channel you said mattered while chatter plays on.

It is remembered per **Profile**, like your selection.

### Taking an Avoid back

**AVOID** is the one control whose effect is silence, so a mis-tap looks exactly like a channel
that went quiet. After every avoid a bar appears above the tabs naming what was silenced, with
**UNDO**. It follows you between tabs and lasts a few seconds; undoing also restores a hold the
avoid released.

After that, **AVOIDING _n_** on the Live screen opens the list of everything currently muted —
each with how long it has left, or *until cleared* — and lets you unmute any one of them.
**CLEAR ALL** is still there, at the bottom, where it can no longer be the only option.

### The session log

**SESSION LOG**, beside **RECENT**, is everything you have heard since you opened the app —
much deeper than the five rows on the Live screen. Tap a row to hear it again; press and hold a
row for **Replay**, **Hold this talkgroup**, **Avoid this talkgroup** and **Download**.

It is this session only and lives in your browser — nothing is sent anywhere and nothing
survives a reload. Everything older than that is in **Search**, which is the Archive.

### Turning the feed off, versus pausing it

**Pause** stops the sound. Everything else carries on: Calls keep arriving, the queue keeps
filling, and when you resume you are behind by however long you paused.

**LIVE FEED off** stops everything. The Call playing stops, the queue empties, and the
connection to the server closes — so an off feed costs no data and no battery, which matters on
a phone. The header reads **FEED OFF** with an amber dot, so you can always tell "I switched
this off" from **NO LINK**, which means the server went away.

Two things follow from it being a real off:

- **Turning it back on starts from now.** The traffic you missed is not replayed — that silence
  was the point. Whatever is happening when you switch back on is what you hear, and the archive
  under **Search** still has the rest.
- **Nothing reaches you while it is off.** Radio-Scout does not send notifications of any kind,
  so switching the feed back on is the only way back in. What happened meanwhile is in
  **Search**.

Your choice is remembered per **Profile** (the `?id=` in the URL), so reloading the page does
not blast you with audio you switched off. Anyone who never touches the toggle gets the feed
live and playing, as before.

### What the dot is telling you

The header always names *why* the feed is or is not playing, because those reasons call for
different reactions — and only some of them are yours to fix:

| Dot | Reads | What it means |
| --- | --- | --- |
| Green, pulsing | **connected** | Calls are arriving. This is the only green there is. |
| Red, pulsing | **linking…** | Connecting, or reconnecting after a drop. Wait. |
| Red, steady | **NO LINK** | The server is not reachable. It keeps retrying; a brief gap is filled in for you when it comes back. |
| Amber, steady | **FEED OFF** | You switched the feed off. Nothing is arriving, by your choice. |
| Amber, steady | **PLAYBACK** | You are playing the archive, which the live feed is mutually exclusive with. |

The two amber states are the two silences you asked for; the red ones are the two you didn't.
When nothing is playing, the panel spells the same thing out in words and says what to do about
it.

The Live screen's controls follow the dot: with the feed off or the archive playing, the
per-Call controls are out of reach rather than present and inert. The **LIVE FEED** switch stays
usable throughout — on **FEED OFF** it is the way back, and on **PLAYBACK** it still reads *on*,
because the feed was never switched off: playback has simply borrowed the audio. The way out of
playback is its own button, **BACK TO LIVE**, which appears right under the switch.

**The last Call stays on the display after it ends**, dimmed and marked `ENDED`, until the next
one arrives. That is deliberate: a scanner's readout does not blank the moment a channel unkeys,
and *after* the transmission is exactly when you reach for **Avoid** — so Hold, Avoid and Replay
go on acting on the Call in front of you. Skip and Pause do not, because there is no audio left
to skip or pause.

### The strip above the tabs

Every screen except **Live** carries a docked strip saying what the app is doing, because Live is
the only one with a player on it and leaving that screen used to take the truth with it. It shows
whichever of two things applies:

- **What is playing** — the Talkgroup, whose audio it is (`Live`, `Interrupting live feed`, or
  where you are in a playback run), and pause and skip. The full set of controls stays on Live.
- **Why nothing is** — `FEED OFF` or `PLAYBACK`, with the one tap that undoes it.

When the feed is simply quiet, or the connection has briefly dropped, the strip says nothing:
neither is something you can act on, and a bar on every screen for a lull is a bar you learn to
ignore.

**Hold and Avoid are opposites, and both are temporary.** Hold means "only this"; Avoid means
"anything but this". A timed Avoid is the one to reach for when a Talkgroup is having a busy
half hour but you do not want to forget you silenced it — which is exactly how a permanent mute
turns into missing something a week later.

---

## Talkgroups

<img src="images/talkgroups.png" alt="The Talkgroups screen" width="620">

What you hear. Everything the instance has ever received a Call for appears here, because
Systems and Talkgroups are created automatically the first time they are heard — nobody has to
configure a list up front.

Three ways to pick, and they compose:

- **Groups** — cross-system categories like Fire, Law, EMS. Tap one to switch every Talkgroup
  in it on or off at once. The counter (`2/2`) shows how many of its Talkgroups are currently on.
- **Tags** — the single service label each Talkgroup carries, like *Fire Dispatch* or
  *Law Talk*. Same bulk behaviour.
- **Individually** — the checkboxes below, grouped by System, with each Talkgroup's TGID
  on the right. The filter box matches on name, tag or TGID.

**ALL ON** / **ALL OFF** and the filter stay at the top of the screen as you scroll, and each
System has its own **ALL OFF** beside its name — so on a county-sized list the controls are
never at the bottom of four hundred rows.

### At county scale

A big system is hundreds of Talkgroups, and the panel is built for that:

- **Pin** the channels you actually listen to — the pin at the end of a row — and they sit in
  their own section at the very top, whichever System they belong to. Pinned rows stay in their
  System too; a pin changes where a Talkgroup is shown and nothing about what plays.
- **Priority** — the ⚡ beside the pin — is the opposite: it changes nothing about where the row
  is drawn, and everything about when its Calls play. See [Priority](#priority).
- **Fold a System away** by tapping its name. A System with more than fifty Talkgroups starts
  folded, so what you see first is a short list of Systems with their counts and controls. Open
  ones stay open next time.
- **Sort** with **A–Z** (the order the operator's labels give) or **ACTIVE** (busiest first).
  Each row shows how long ago that channel was last heard — `3m`, `2h` — or, sorted by activity,
  how many Calls it has taken lately. The ACTIVE button names the window it counts over.
- **Typing in the filter opens everything**, so a search is never answered by a folded box.

Your selection, your pins, your priorities, your sort and which Systems you folded away are
saved in this browser and survive a reload.

### Two independent setups in one browser

Add `?id=` and a name to the URL:

```
http://<host>:3000/?id=truck
http://<host>:3000/?id=desk
```

Each name is a separate **Profile** with its own selection, avoids, holds and priorities —
nothing is shared between them. Bookmark each one, or install them as two home-screen apps. With no `?id=`
you get the default Profile.

---

## Search

<img src="images/search.png" alt="The Search screen" width="620">

The Archive: every Call the instance still holds, however you selected the live feed. Filter by
time range, System, Talkgroup, Group and Tag, and sort newest or oldest first. **MARK** narrows to
the Calls something was said about — the emergencies, or the station page-outs. *Archive spans*
tells you how far back the instance's history actually goes, which is decided by its retention
policy.

Every result shows its duration, in a column down the right — so a one-second kerchunk and a
forty-second dispatch are told apart without playing either. **MIN DURATION** filters the short
ones out entirely, which is the fastest way to make a busy day readable.

Two things to know about that column. A dash means nobody measured it: Calls stored by an older
version of Radio-Scout carry no duration, and neither does audio whose header could not be read.
And because an unknown duration cannot be compared against a threshold, those Calls do not match
**MIN DURATION** at any setting — leave it on *Any duration* to see them.

Beside it is a **unit** column: who keyed it, where anybody knows. Radios name themselves over the
air, so "MEDIC 7" turns up beside a Call without your operator configuring anything — and where
nobody has named one, its radio id is there instead, which is still enough to tell two radios apart.
Tap it to open that radio's history: which talkgroups it uses, when it was first and last heard, and
its calls. **UNIT** filters the search to one radio directly, if you have the number — and a fleet
whose operator has grouped its radios answers as the whole apparatus, not just the one you typed.

Each result plays in place, or downloads with the arrow. Encrypted Calls have neither button:
they are metadata-only records, with a 🔒 badge and no audio behind them. **PLAYBACK MODE**, top right, switches
from the live feed to playing the search results in sequence — for working through an incident
after the fact rather than waiting on what arrives next. Live feed and playback mode are
mutually exclusive: you are in one or the other, and either the strip above the tabs or the
**BACK TO LIVE** button on the Live screen takes you back.

---

## On your phone

This is the part rdio-scanner does badly, and the reason a lot of this project exists.

### Install it

**iOS/iPadOS (Safari):** Share → **Add to Home Screen**. It must be Safari; other iOS browsers
cannot install a web app.

**Android (Chrome):** an install prompt appears, or menu → **Install app**.

Installing gives you a real app icon, no browser chrome, and — on iOS — the only reliable way
to get background audio. The app shell also works offline: it opens and shows the interface
without a connection. Audio itself needs the server, since Calls are not cached.

### Background audio and the lock screen

Play a Call, lock the phone, and audio keeps going. The lock screen and Control Centre get
working transport controls — play/pause and next — with the Talkgroup shown as the track. It
works because audio is served as real URLs to a real `<audio>` element driven by the Media
Session API, which is the arrangement iOS honours; rdio-scanner's WebAudio approach is
suspended by iOS the moment you put the phone away.

If audio stops when you lock the phone, the usual cause is that you are running from the
browser rather than the installed app.

### No notifications

**Radio-Scout does not notify you.** It never asks for notification permission and never wakes
your device — there is nothing to switch on. Earlier releases (0.1.x) had Web Push and it was
removed in 0.2.0; if your phone used to buzz for a Talkgroup and has stopped, that is why, not
a fault. The reasoning is in [ADR-0014](adr/0014-no-notifications.md).

What this means in practice: hearing a Call means having the app open with the feed on. What
you missed while you were away is in **Search**, which is where it always was.

---

## Settings

<img src="images/settings.png" alt="The Settings screen" width="620">

Currently: whether the server is reachable.

**Audio enhancement**, **Theme** and **Admin** are listed but not built yet — they read "soon"
because that is honest. Enhancement *works*, but it is configured on the server rather than per
listener; see [operating.md](operating.md#audio-enhancement).

Almost nothing else is a per-listener setting by design: what the instance does is the
operator's TOML file, not a preference panel.

---

## Troubleshooting

**"unreachable" on the Settings screen, or the Live dot is not green.** The server is down or
you have no route to it. The app shell still loads from cache when installed, which is why you
can see this message at all.

**Nothing plays, but Calls are arriving.** Check the Talkgroups screen — a Talkgroup that is
switched off, or Avoided, is silently skipped. `ALL ON` is the quick test.

**Audio stops when the phone locks (iOS).** Use the installed app, not Safari with a tab open.

**The queue keeps growing.** More is arriving than plays in real time. Narrow the selection, or
use SKIP — the queue is a backlog of things you have not heard, not a buffer that drains on its
own.

**A Talkgroup went quiet and you don't know why.** You probably Avoided it. Avoids without a
timer stay until you clear them.
