// Radio-Scout's Trunk Recorder plugin — the upload core.
//
// Deliberately free of every Trunk Recorder header. Nothing here knows what a
// `Call_Data_t` is; it takes strings and file paths, so it can be compiled and
// driven without a recorder at all — which is what `tests/trplugin.rs` does,
// against a real Radio-Scout over real HTTP. The TR-facing shim in
// `radio_scout_uploader.cc` is the only translation unit that includes the
// recorder's headers, and it is a thin mapping onto this interface.
//
// Part of Radio-Scout. https://github.com/FxllenCode/radio-scout

#ifndef RADIO_SCOUT_UPLOAD_H
#define RADIO_SCOUT_UPLOAD_H

#include <string>
#include <vector>

namespace radio_scout {

// How long one upload may take by default, and the value the plugin's
// `timeoutSecs` setting falls back to. Named once so the header, the plugin and
// the guide cannot each carry their own number.
constexpr long kDefaultTimeoutSecs = 60;

// Which talkgroups this system sends, as the glob patterns an operator writes
// in `talkgroupAllow` / `talkgroupDeny` — the same spelling the
// `rdioscanner_uploader` plugin takes (`rdioscanner_uploader.cc:603`), so a
// configuration migrating across does not lose it.
//
// Matched as globs directly rather than compiled to a regex: `*` and `?` mean
// what they mean, and every other character — `.` above all — is itself,
// because it never becomes a metacharacter that has to be escaped back.
struct TalkgroupFilter {
  std::vector<std::string> allow;
  std::vector<std::string> deny;

  // Empty lists admit everything. A non-empty `allow` is exhaustive: a
  // talkgroup matching none of it is not sent. `deny` then removes from
  // whatever is left, so denying beats allowing.
  bool admits(long talkgroup) const;
};

// One finished Call, as the plugin sends it.
struct Upload {
  // The instance's base address. The path is ours to append, so an operator
  // configures the same bare URL the rdio-scanner uploader takes.
  std::string server;
  std::string api_key;
  // Trunk Recorder's own call JSON, verbatim — `Call_Data_t::call_json`, the
  // object `create_call_json` just wrote beside the audio. Sending it
  // unmodified is why there is no field mapping here to drift from the parser.
  std::string meta_json;
  // The audio file to send — see `audio_to_send` for which of the two it is.
  std::string audio_path;
  // How long one upload may take, start to finish, before it is abandoned and
  // handed back to Trunk Recorder's retry.
  //
  // There has to be a bound. A server that accepts a connection and then says
  // nothing — a half-open NAT, a wedged proxy — would otherwise park a
  // call-data worker forever; Trunk Recorder polls those futures from its main
  // loop and waits on them at shutdown, so a black hole leaks a thread per Call
  // and then hangs the recorder on the way out.
  long timeout_secs = kDefaultTimeoutSecs;
};

// What became of it.
struct Result {
  // Radio-Scout answered, and answered 2xx.
  bool sent = false;
  // Nothing was attempted: there is no address or no key to attempt it against.
  //
  // Worth its own answer rather than a failed transfer, because Trunk Recorder
  // **discards what `parse_config` returns** (`plugin_manager.cc:56`) — a
  // plugin that reports its configuration unusable is loaded and handed every
  // Call regardless. Reported as a failure, each of those would cost the
  // recorder its full retry budget, on every Call, for as long as the typo
  // lasted.
  bool unconfigured = false;
  // The status, when there was one. Zero means nobody answered.
  long http_code = 0;
  // The server's own words, which are the rdio-compatible response strings.
  std::string body;
  // What libcurl said went wrong, when nothing answered. Never carries the API
  // key: it is a mime part, not part of the URL or a header.
  std::string transport_error;
};

// Which of the two files Trunk Recorder left on disk to send: the compressed
// copy when `compressWav` made one, else the rendered WAV.
//
// The rule lives here rather than in the shim so it is the same rule the tests
// drive. Trunk Recorder's own rdio uploader chooses on the flag too, not on
// which file happens to exist (`rdioscanner_uploader.cc:334`) — `converted` is
// only ever populated when the conversion ran.
std::string audio_to_send(const std::string &rendered, const std::string &compressed,
                          bool compress_wav);

// POST one Call to `<server>/api/trunk-recorder-call-upload`.
Result send(const Upload &upload);

} // namespace radio_scout

#endif
