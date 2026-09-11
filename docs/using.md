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

### Catching up

**CATCH UP** is the other way out of a backlog, and the one that costs you nothing. Instead of
giving the queue up, it drains it: the stretches where nobody is talking are skipped, and what is
left plays at 1.5×. Every word still reaches you, sooner.

The button says what it is worth before you press it — *0:52 instead of 4:20* — and counts down
once it is running. A **1.5×** marker appears beside the **Q** count so the screen never claims
to be doing something it isn't.

It stops on its own the moment the queue is empty. There is nothing to catch up on then, and the
call playing is the newest there is, so it plays at normal speed. You can also stop it by pressing
the button again.

Two things worth knowing:

- **Where the gaps come from.** Your instance looks at each call's audio once, in the background,
  and works out where the silence is. A recorder's file usually spans a whole grant, so it holds
  the pauses between one unit letting go and the next keying — which is why the trim is often
  worth more than the speed.
- **It still helps if it hasn't looked.** A call the instance has not scanned — or an instance
  where the operator turned scanning off — simply plays at 1.5× with nothing trimmed. Nothing
  breaks, and nothing is skipped that shouldn't be.

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

Each result plays in place, or downloads with the arrow. Encrypted Calls have no play or download
button: they are metadata-only records, with a 🔒 badge and no audio behind them. **PLAYBACK MODE**, top right, switches
from the live feed to playing the search results in sequence — for working through an incident
after the fact rather than waiting on what arrives next. Live feed and playback mode are
mutually exclusive: you are in one or the other, and either the strip above the tabs or the
**BACK TO LIVE** button on the Live screen takes you back.

Beside play is a second button — ⏩ — that plays *forward in time* from that Call instead. The list
is newest-first, so ordinary play walks backwards through history; this walks the other way, from
the moment you tapped through everything that came after it. It does not disturb the list you are
reading: the results stay sorted as you left them, and the run carries on through later pages on
its own.

### The activity chart

Above the results is a bar per slice of time, across the whole of whatever the current filters
reach — a picture of when it was busy. Set a Talkgroup filter and it becomes that channel's: this is
where "when does Fire Dispatch actually run" is a shape rather than a guess.

**Drag along it to travel.** The date under your thumb is shown as you move, and the results jump
there when you let go — one gesture instead of forty taps on *Next page*. Letting go is what moves
them on purpose: a list that reloaded under your thumb for every bar you crossed would be a hundred
searches for one journey, which on a Pi you would feel. The filters do not change and neither does
anything you have playing. The bar in white is where the page on screen is, and it follows you as
you page normally. Arrow keys work too, with Home and End for either end of the range.

**BY HOUR** flips the same numbers into a week: seven rows, twenty-four columns, darker where it is
busier. That answers the other question — not when it *was* busy, but when it *usually* is. It reads
in your own clock, wherever the instance happens to be, and it covers the last four weeks of
whatever you have filtered to.

Both are drawn from the same filters as the results, so the bars always add up to the count above
them. rdio-scanner has neither: time travel there is the Previous button, one page at a time.

### The DVR

**DVR**, beside the presets, opens the same archive as something to *play* rather than something to
read: pick a channel — or your whole scanner — pick a stretch of time, and it plays that stretch
forwards, oldest first, straight through. "Rewind the county to 2am last Friday" is one screen.

It opens with whatever you had already filtered, which is the point: filter to Fire Dispatch and
last night, press **DVR**, press play, and you are listening to last night on Fire Dispatch.

The timeline across the top is the same activity chart, and here the marker is *where you are* —
it moves as the calls play. Drag it and playback moves with it, which is the difference from the
Search screen's chart: there, dragging moves the page you are reading and leaves what you are
playing alone; here, dragging is rewinding.

The slider under the transport seeks *inside* the call playing, so a long dispatch can be moved
about in without skipping past it. **1.5× · SKIP QUIET** does both of the things catching up on the
live queue does: raises the speed with speech still intelligible, and jumps the stretches nobody is
talking in. Most recorder files end in several seconds of dead air after the last word; skipping it
is most of why an hour of traffic takes far less than an hour to get through.

Two things it deliberately does not do. It never plays backwards — a run that went the other way
would be a search result, not a DVR. And it plays what your *selection* would have played, patches
included: if a channel was patched onto another that night, you hear the traffic that reached it.

The speed setting belongs to the DVR run and nothing else: it does not follow you back to the live
feed, and catching up on the live queue does not start a DVR already at 1.5×.

The 🔗 button shares the whole thing — the scope, the range and where you had rewound to.

### Date presets

**LAST HOUR**, **TODAY**, **YESTERDAY** and **LAST 7 DAYS** fill both date boxes in one tap, and
**RESET** clears every filter at once. The presets fill in real dates rather than staying "the last
hour" forever — so what you end up with is a fixed window you can read, keep and send, and the two
date boxes always show exactly what is being searched.

### Every view is a link

The address bar carries the whole search: every filter, the sort, and which page you are on. So a
search is a bookmark, the browser's back and forward buttons walk your searches, a reload lands you
where you were, and switching to another tab and back does not throw your filters away.

Three 🔗 buttons hand a link to your phone's share sheet, or copy it if there isn't one:

| Where | What the link opens |
| --- | --- |
| Beside the filters | This search, filters and page and all |
| On a result row | That one Call, playing, whatever the recipient was looking at |
| On the Talkgroups panel | Your selected talkgroups, applied to their scanner |

A Call link works even if the person opening it has different filters set, or none — it names the
Call, not a search that happens to contain it.

A **selection** link replaces what the person opening it is listening to, so it comes with an
**UNDO** for a few seconds — theirs is not lost by opening yours. What travels is the whole
selection, defaults included, so a talkgroup neither of you has heard of yet behaves the same way on
both scanners. It is not a **Profile**: their pins, avoids and holds are untouched, and if you want
a second scanner rather than a changed one, use `?id=` above.

### Starring what mattered

The ☆ keeps a call. It is on every list of calls you have heard or found — **Search** results, the
**RECENT** list on Live, a radio's history, the **DVR** — on the Live screen's control grid beside
**PRIORITY**, so you can keep a call while you are still hearing it, and on the player itself while
you are walking search results. On the **session log** it is in the press-and-hold menu, beside
Replay and Hold. **STARRED** in the filters is then how you get back to them, and it combines with
everything else: starred calls on Fire Dispatch last Tuesday is one search.

Two places deliberately have no star. The **queue** sheet lists calls you have not heard yet, and
there is nothing to decide about one of those. And the strip above the tabs carries pause and skip
only — three buttons in a docked strip is a row of targets too small to hit on a phone — so keeping
what you are hearing from another tab is one tap through to **Live**, where the display is still
showing it.

Two things about it are worth knowing, and they are unusual enough to say out loud.

- **A star belongs to the instance, not to your browser.** Anybody listening can leave one, and
  everybody sees the same starred list. There are no accounts here, so the alternative would have
  been for the server to keep a record of what each browser kept — which is exactly the kind of
  record this project does not accumulate. The trade is that the starred list is shared. On a
  scanner with a handful of listeners that tends to be the point.
- **Whether it outlasts the retention window is the operator's call**, and the line under the
  **STARRED** filter says which way it is set here: kept for so many days, kept indefinitely, or not
  kept at all. On an instance that has not turned it on, a star is a bookmark — useful for finding
  something again this week, and no protection against the archive rolling over.

Encrypted calls can be starred too. There is nothing to play, but the record that the channel was
busy at that moment is often exactly what an incident is assembled out of.

### Sending a call to someone who does not use this

All three of those links open the app. The ⤴ button — on a search result row, and in the
press-and-hold menu on the **session log**, which is where you go when you just *heard* something —
does something different: it mints a **public link** to that one call — a plain page with the call on it and a play button, which
works in any browser, on any phone, for somebody who has never heard of this instance. Paste it into
a message and it comes up as a preview card naming the talkgroup, the system and when it was.

Three things are worth knowing:

- **It reaches that one call and nothing else.** No search, no talkgroup list, no other audio.
- **It expires.** A week, unless the operator has set something else; after that the page says so.
  Sharing the same call again hands you the same link with a fresh week on it, so you can re-send
  one without collecting a drawer full of URLs.
- **The operator can revoke it**, and can turn the whole feature off — on an instance that has, the
  ⤴ button is simply not there.

An encrypted call can be shared too. There is nothing to play, but that the channel was busy at that
moment is often the point.

### Taking an incident with you

The ⤓ button — beside the link button on **Search**, and on the **DVR** — downloads what you are
looking at. Whatever filters are set is what comes down: a talkgroup, a night, a whole selection,
the lot. Two shapes to choose from:

- **A zip of calls.** Every call as its own file, named `0001-County-Fire-Dispatch-…` so a folder
  of them sorts into the order it happened, plus a `manifest.json` listing every call with its
  talkgroup, system, time, duration, units and which file is which. Encrypted calls are in the
  manifest — the activity is the point — with no file beside them.
- **One stitched file.** All of it end to end, oldest first, as a single WAV you can send to
  somebody who has never heard of Radio-Scout and who will just press play. Calls whose length this
  instance never measured are left out of it, and so are encrypted ones: neither can be placed on a
  timeline.

It starts downloading immediately rather than after a wait — a long range is written as it goes, so
nothing has to be assembled first. There is a limit on how many calls one export may carry (a
thousand, unless your operator has changed it); over it, the control tells you the count instead of
handing you a download that was never going to arrive. Narrow the range and try again.

One export runs at a time on an instance. If somebody else is mid-download you are asked to come
back in a moment, rather than both of you getting a slow one.

### Someone sent you an event

An **Event** is an incident your operator kept: a named collection of calls — the tone-out, the
dispatch, the fireground traffic, the all-clear — assembled by hand and **frozen**, so it is still
there long after retention has taken everything around it.

You cannot make one; only the operator can, because keeping an incident costs their disk for good.
What you can be sent is a link to one, and that link works the way a shared call's does: a plain
page in any browser, on any phone, for somebody who has never heard of this instance. It lists every
call in the incident with a play button on each, and offers the same two downloads a search does —
a zip, or one stitched file.

Two differences from a shared call are worth knowing:

- **It does not expire.** An event is the durable thing by definition, so there is no week on it.
  What there is instead is an operator who can stop sharing it, and if they do the link is dead for
  good rather than paused — re-sharing gives out a different one.
- **A long incident shows the first two hundred calls.** The page says so when it does, and the
  download has all of them.

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
