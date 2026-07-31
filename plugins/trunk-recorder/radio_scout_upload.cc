// Radio-Scout's Trunk Recorder plugin — the upload core. See the header.

#include "radio_scout_upload.h"

#include <curl/curl.h>

#include <mutex>

namespace radio_scout {
namespace {

size_t collect(void *contents, size_t size, size_t count, void *into) {
  const size_t bytes = size * count;
  static_cast<std::string *>(into)->append(static_cast<char *>(contents), bytes);
  return bytes;
}

// `<server>/api/trunk-recorder-call-upload`, whether or not the operator left a
// trailing slash on the address.
std::string endpoint(const std::string &server) {
  std::string base = server;
  while (!base.empty() && base.back() == '/') {
    base.pop_back();
  }
  return base + "/api/trunk-recorder-call-upload";
}

// The MIME type for a file Trunk Recorder rendered, by extension.
//
// Radio-Scout stores what it is told here and serves it back to browsers, so a
// wrong type is a Call that will not play. Anything unrecognised is declared as
// what it certainly is — bytes — rather than guessed at.
std::string mime_for(const std::string &path) {
  const size_t dot = path.rfind('.');
  const std::string extension = dot == std::string::npos ? "" : path.substr(dot);
  if (extension == ".wav") {
    return "audio/wav";
  }
  if (extension == ".m4a" || extension == ".mp4") {
    return "audio/mp4";
  }
  if (extension == ".mp3") {
    return "audio/mpeg";
  }
  return "application/octet-stream";
}

// The name a file is known by, with the directory Trunk Recorder happened to
// write it in left off.
std::string basename_of(const std::string &path) {
  const size_t slash = path.rfind('/');
  return slash == std::string::npos ? path : path.substr(slash + 1);
}

// Whole-string glob match: `*` any run of characters, `?` exactly one, and
// everything else itself. The usual backtracking walk — on a mismatch, fall
// back to the last `*` and let it swallow one more character.
bool glob_match(const std::string &pattern, const std::string &text) {
  size_t p = 0;
  size_t t = 0;
  size_t star = std::string::npos;
  size_t after_star = 0;

  while (t < text.size()) {
    if (p < pattern.size() && (pattern[p] == '?' || pattern[p] == text[t])) {
      ++p;
      ++t;
    } else if (p < pattern.size() && pattern[p] == '*') {
      star = p++;
      after_star = t;
    } else if (star != std::string::npos) {
      p = star + 1;
      t = ++after_star;
    } else {
      return false;
    }
  }
  while (p < pattern.size() && pattern[p] == '*') {
    ++p;
  }
  return p == pattern.size();
}

bool matches_any(const std::vector<std::string> &patterns, const std::string &text) {
  for (const std::string &pattern : patterns) {
    if (glob_match(pattern, text)) {
      return true;
    }
  }
  return false;
}

} // namespace

bool TalkgroupFilter::admits(long talkgroup) const {
  const std::string value = std::to_string(talkgroup);
  if (!allow.empty() && !matches_any(allow, value)) {
    return false;
  }
  return !matches_any(deny, value);
}

std::string audio_to_send(const std::string &rendered, const std::string &compressed,
                          bool compress_wav) {
  return compress_wav && !compressed.empty() ? compressed : rendered;
}

Result send(const Upload &upload) {
  Result result;

  if (upload.server.empty() || upload.api_key.empty()) {
    result.unconfigured = true;
    return result;
  }

  // Trunk Recorder concludes each Call on a worker thread of its own, so the
  // first `curl_easy_init` can happen on several at once. libcurl's implicit
  // initialisation only became thread-safe in 7.84, and a Raspberry Pi OS
  // release older than that is exactly the machine this runs on.
  static std::once_flag curl_started;
  std::call_once(curl_started, [] { curl_global_init(CURL_GLOBAL_DEFAULT); });

  CURL *curl = curl_easy_init();
  if (curl == nullptr) {
    result.transport_error = "curl_easy_init failed";
    return result;
  }

  curl_mime *mime = curl_mime_init(curl);

  curl_mimepart *part = curl_mime_addpart(mime);
  curl_mime_name(part, "key");
  curl_mime_data(part, upload.api_key.c_str(), upload.api_key.size());

  part = curl_mime_addpart(mime);
  curl_mime_name(part, "meta");
  curl_mime_data(part, upload.meta_json.c_str(), upload.meta_json.size());

  const std::string audio_name = basename_of(upload.audio_path);
  const std::string audio_mime = mime_for(upload.audio_path);
  part = curl_mime_addpart(mime);
  curl_mime_name(part, "audio");
  curl_mime_filedata(part, upload.audio_path.c_str());
  curl_mime_filename(part, audio_name.c_str());
  curl_mime_type(part, audio_mime.c_str());

  const std::string url = endpoint(upload.server);
  char error_buffer[CURL_ERROR_SIZE];
  error_buffer[0] = '\0';

  curl_easy_setopt(curl, CURLOPT_URL, url.c_str());
  curl_easy_setopt(curl, CURLOPT_MIMEPOST, mime);
  curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, collect);
  curl_easy_setopt(curl, CURLOPT_WRITEDATA, &result.body);
  curl_easy_setopt(curl, CURLOPT_ERRORBUFFER, error_buffer);
  curl_easy_setopt(curl, CURLOPT_TIMEOUT, upload.timeout_secs);

  const CURLcode transfer = curl_easy_perform(curl);
  if (transfer == CURLE_OK) {
    curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &result.http_code);
    result.sent = result.http_code >= 200 && result.http_code < 300;
  } else {
    result.transport_error =
        error_buffer[0] != '\0' ? error_buffer : curl_easy_strerror(transfer);
  }

  curl_mime_free(mime);
  curl_easy_cleanup(curl);
  return result;
}

} // namespace radio_scout
