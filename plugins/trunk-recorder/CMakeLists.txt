# Radio-Scout's Trunk Recorder plugin.
#
# Drop this directory into Trunk Recorder's `user_plugins/` and rebuild: the
# top-level CMakeLists globs `user_plugins/*/CMakeLists.txt` and adds each one
# it finds (`CMakeLists.txt:393-403`). Installation, and the `plugins` entry
# that loads the result, are in Radio-Scout's docs/recorders.md.

add_library(radio_scout_uploader
  MODULE
    radio_scout_uploader.cc
    radio_scout_upload.cc
  )

# Deliberately shorter than the link line every built-in uploader carries.
#
# `trunk_recorder_library` is a STATIC archive with no link interface of its
# own, which is why those plugins each repeat the whole GNU Radio list after it.
# This plugin calls no Trunk Recorder function — the upload core talks to
# libcurl and the shim only reads fields off a `Call_Data_t` — so the linker
# pulls no member out of that archive and needs none of GNU Radio to satisfy
# it. It stays named so a build still orders the two correctly, and so an
# undefined symbol would surface here rather than at `dlopen` on somebody's
# recorder.
target_link_libraries(radio_scout_uploader
  trunk_recorder_library
  ${CURL_LIBRARIES}
  ${Boost_LIBRARIES}
  )

install(TARGETS radio_scout_uploader
  LIBRARY DESTINATION ${CMAKE_INSTALL_PREFIX}/lib/trunk-recorder)
