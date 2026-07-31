// A `main()` for the plugin's upload core, so `tests/trplugin.rs` can drive it
// without Trunk Recorder — it takes the same values `call_end` reads off a
// `Call_Data_t`, calls the same `radio_scout::send`, and exits with what
// `call_end` would return (0 = done with this Call, 1 = Trunk Recorder should
// retry it).
//
// Trunk Recorder never compiles this file — `CMakeLists.txt` names the two
// beside it one by one — so it is inert in an operator's build. It ships with
// the plugin anyway, because it is the executable description of how the thing
// is exercised.

#include "radio_scout_upload.h"

#include <fstream>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

namespace {

std::string read_file(const std::string &path) {
  std::ifstream file(path, std::ios::binary);
  std::ostringstream contents;
  contents << file.rdbuf();
  return contents.str();
}

} // namespace

int main(int argc, char *argv[]) {
  radio_scout::Upload upload;
  std::string meta_path;
  std::string wav_path;
  std::string m4a_path;
  bool compress_wav = false;
  radio_scout::TalkgroupFilter filter;
  long talkgroup = 0;

  const std::vector<std::string> args(argv + 1, argv + argc);
  for (size_t i = 0; i < args.size(); ++i) {
    const std::string &flag = args[i];
    const bool has_value = i + 1 < args.size();
    if (flag == "--server" && has_value) {
      upload.server = args[++i];
    } else if (flag == "--key" && has_value) {
      upload.api_key = args[++i];
    } else if (flag == "--meta" && has_value) {
      meta_path = args[++i];
    } else if (flag == "--wav" && has_value) {
      wav_path = args[++i];
    } else if (flag == "--m4a" && has_value) {
      m4a_path = args[++i];
    } else if (flag == "--compress") {
      compress_wav = true;
    } else if (flag == "--timeout" && has_value) {
      upload.timeout_secs = std::stol(args[++i]);
    } else if (flag == "--talkgroup" && has_value) {
      talkgroup = std::stol(args[++i]);
    } else if (flag == "--allow" && has_value) {
      filter.allow.push_back(args[++i]);
    } else if (flag == "--deny" && has_value) {
      filter.deny.push_back(args[++i]);
    } else {
      std::cerr << "harness: unknown argument " << flag << "\n";
      return 2;
    }
  }

  if (!filter.admits(talkgroup)) {
    std::cout << "skipped: talkgroup " << talkgroup << " is filtered out\n";
    return 0;
  }

  upload.meta_json = read_file(meta_path);
  upload.audio_path = radio_scout::audio_to_send(wav_path, m4a_path, compress_wav);

  const radio_scout::Result result = radio_scout::send(upload);
  if (result.unconfigured) {
    std::cout << "not configured: "
              << (upload.server.empty() ? "no server" : "no apiKey") << "\n";
    return 0;
  }
  if (result.sent) {
    std::cout << "sent (HTTP " << result.http_code << "): " << result.body;
    return 0;
  }
  if (result.http_code != 0) {
    std::cout << "refused (HTTP " << result.http_code << "): " << result.body;
  } else {
    std::cout << "failed: " << result.transport_error << "\n";
  }
  return 1;
}
