// Radio-Scout's Trunk Recorder plugin.
//
// Posts each finished Call to `POST /api/trunk-recorder-call-upload` — the
// endpoint that takes Trunk Recorder's own `.wav` + `.json` pair rather than
// the rdio-scanner dialect. That dialect has no field for the emergency and
// encrypted flags, the exact call length, the stop time, the priority, the
// audio type, the per-frequency decode health, or the aliases radios put over
// the air, so the `rdioscanner_uploader` plugin discards all of it at the door.
// This sends the recorder's own metadata object untouched.
//
// **This file is the only one here that includes a Trunk Recorder header**, and
// it holds no decisions: which file to send, which talkgroups to send, and how
// to send them all live in `radio_scout_upload.cc`, which compiles without a
// recorder and is driven directly by `tests/trplugin.rs` against a real
// Radio-Scout. What is left is the mapping from `Call_Data_t`.
//
// Part of Radio-Scout. https://github.com/FxllenCode/radio-scout

#include "radio_scout_upload.h"

#include "../../trunk-recorder/plugin_manager/plugin_api.h"

#include <boost/dll/alias.hpp>
#include <boost/log/trivial.hpp>
#include <boost/shared_ptr.hpp>

#include <string>
#include <vector>

namespace {

// One `systems` entry: what this recorder's system is called, the key it
// uploads with, and which of its talkgroups leave the box.
struct Configured_System {
  std::string short_name;
  std::string api_key;
  radio_scout::TalkgroupFilter filter;
};

// Read a `talkgroupAllow` / `talkgroupDeny` array. An absent key is no filter.
//
// A key that is present but not an array is a typo, and the honest outcome is
// the loud one: it becomes no filter, which means *everything* is sent, so the
// operator has to be told rather than left to notice the bandwidth. Refusing to
// load instead is not on the table — Trunk Recorder discards what
// `parse_config` returns (`plugin_manager.cc:56`).
std::vector<std::string> read_globs(const json &system, const char *key,
                                    const std::string &short_name) {
  std::vector<std::string> globs;
  if (!system.contains(key)) {
    return globs;
  }
  const json &value = system.at(key);
  if (!value.is_array()) {
    BOOST_LOG_TRIVIAL(error) << "\t[Radio-Scout]\t" << short_name << ": " << key
                             << " must be an array of patterns, e.g. [\"541*\"] — ignoring it, "
                                "so every talkgroup on this system will be uploaded";
    return globs;
  }
  for (const json &glob : value) {
    if (glob.is_string()) {
      globs.push_back(glob.get<std::string>());
    } else {
      globs.push_back(glob.dump());
    }
  }
  return globs;
}

} // namespace

class Radio_Scout_Uploader : public Plugin_Api {
  std::string server;
  std::string api_key;
  long timeout_secs = radio_scout::kDefaultTimeoutSecs;
  std::vector<Configured_System> systems;
  std::string plugin_name = "Radio-Scout";

  const Configured_System *configured(const std::string &short_name) const {
    for (const Configured_System &system : systems) {
      if (system.short_name == short_name) {
        return &system;
      }
    }
    return nullptr;
  }

  std::string log_prefix(const Call_Data_t &call_info) const {
    return "\t[" + plugin_name + "]\t" + call_info.short_name + " TG " +
           std::to_string(call_info.talkgroup) + "\t";
  }

public:
  int parse_config(json config_data) override {
    plugin_name = config_data.value("name", std::string("Radio-Scout"));

    server = config_data.value("server", std::string(""));
    if (server.empty()) {
      BOOST_LOG_TRIVIAL(error) << "\t[" << plugin_name
                               << "]\tNo \"server\" configured — set it to the instance's base "
                                  "address, e.g. http://scout.lan:3000";
      return 1;
    }

    api_key = config_data.value("apiKey", std::string(""));
    if (api_key.empty()) {
      // Named, never printed: an API key does not belong in a log at any level
      // (Radio-Scout's ADR-0011 rule 2, and a recorder's log is a log).
      BOOST_LOG_TRIVIAL(error) << "\t[" << plugin_name
                               << "]\tNo \"apiKey\" configured — it is the key Radio-Scout wrote "
                                  "into its .env on first run";
      return 1;
    }

    timeout_secs = config_data.value("timeoutSecs", radio_scout::kDefaultTimeoutSecs);

    if (config_data.contains("systems") && !config_data.at("systems").is_array()) {
      BOOST_LOG_TRIVIAL(error) << "\t[" << plugin_name
                               << "]\t\"systems\" must be an array — ignoring it, so every "
                                  "system uploads with the key above and no filter";
    }
    if (config_data.contains("systems") && config_data.at("systems").is_array()) {
      for (const json &entry : config_data.at("systems")) {
        Configured_System system;
        system.short_name = entry.value("shortName", std::string(""));
        // Per-system keys are optional here, unlike the rdio uploader: the
        // native endpoint files a Call under the System it resolves from
        // `short_name`, so one instance-wide key is the normal case and a
        // per-system one is the exception.
        system.api_key = entry.value("apiKey", api_key);
        system.filter.allow = read_globs(entry, "talkgroupAllow", system.short_name);
        system.filter.deny = read_globs(entry, "talkgroupDeny", system.short_name);
        systems.push_back(system);
      }
    }

    BOOST_LOG_TRIVIAL(info) << "\t[" << plugin_name << "]\tUploading to " << server
                            << "/api/trunk-recorder-call-upload";
    return 0;
  }

  // Trunk Recorder has finished a Call and written its metadata JSON.
  //
  // Returning non-zero puts **this plugin alone** on the Call's retry list
  // (`plugin_manager.cc:194-198`): Trunk Recorder keeps the files, backs off,
  // and re-runs `call_end` for the failed plugins only (`:203-205`). Every
  // other uploader on the recorder is untouched either way — which is what the
  // shipped `uploadScript` cannot say, and the reason this plugin is allowed to
  // fail a Call at all.
  int call_end(Call_Data_t call_info) override {
    const Configured_System *system = configured(call_info.short_name);

    if (system != nullptr && !system->filter.admits(call_info.talkgroup)) {
      BOOST_LOG_TRIVIAL(debug) << log_prefix(call_info) << "not uploaded: talkgroup filter";
      return 0;
    }

    radio_scout::Upload upload;
    upload.server = server;
    upload.api_key = system != nullptr ? system->api_key : api_key;
    // Trunk Recorder's own call JSON, exactly as `create_call_json` built it a
    // moment ago (`call_concluder.cc`) — nothing here re-serialises it, so
    // there is no second definition of the payload to drift from the parser.
    upload.meta_json = call_info.call_json.dump();
    upload.audio_path = radio_scout::audio_to_send(call_info.filename, call_info.converted,
                                                   call_info.compress_wav);
    upload.timeout_secs = timeout_secs;

    const radio_scout::Result result = radio_scout::send(upload);
    if (result.unconfigured) {
      // Reachable because Trunk Recorder ignores what `parse_config` returned
      // (`plugin_manager.cc:56`). DEBUG, not ERROR: `parse_config` already said
      // this once at startup, and a line per Call would bury it.
      BOOST_LOG_TRIVIAL(debug) << log_prefix(call_info) << "not uploaded: not configured";
      return 0;
    }
    if (result.sent) {
      BOOST_LOG_TRIVIAL(info) << log_prefix(call_info) << "uploaded";
      return 0;
    }
    if (result.http_code != 0) {
      // Radio-Scout's own words: the rdio-compatible response strings, which
      // say which of the two API keys was refused and for what.
      BOOST_LOG_TRIVIAL(error) << log_prefix(call_info) << "upload refused (HTTP "
                               << result.http_code << "): " << result.body;
    } else {
      BOOST_LOG_TRIVIAL(error) << log_prefix(call_info) << "upload failed: "
                               << result.transport_error;
    }
    return 1;
  }

  static boost::shared_ptr<Radio_Scout_Uploader> create() {
    return boost::shared_ptr<Radio_Scout_Uploader>(new Radio_Scout_Uploader());
  }
};

// The symbol Trunk Recorder looks for in every plugin
// (`plugin_manager.cc:23-27`), by that exact name.
BOOST_DLL_ALIAS(Radio_Scout_Uploader::create, create_plugin)
