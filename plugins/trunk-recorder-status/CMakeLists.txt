# Radio-Scout's Trunk Recorder status plugin (#71).
#
# Drop this directory into Trunk Recorder's `user_plugins/` and rebuild: the
# top-level CMakeLists globs `user_plugins/*/CMakeLists.txt` and adds each one it
# finds (`CMakeLists.txt:392-403`). Installation, and the two lines of
# `config.json` that load and point it, are in Radio-Scout's docs/recorders.md.
#
# The link line is upstream `stat_socket`'s and is long for a reason this plugin
# genuinely has and the *uploader* plugin does not: it calls Trunk Recorder's own
# `Source`, `System`, `Call` and `Recorder` methods, so the linker really does
# pull members out of `trunk_recorder_library` and really does need what those
# members were built against.

add_library(radio_scout_status
  MODULE
    radio_scout_status.cc
  )

target_link_libraries(radio_scout_status
  trunk_recorder_library
  ssl
  crypto
  ${Boost_LIBRARIES}
  ${GNURADIO_PMT_LIBRARIES}
  ${GNURADIO_RUNTIME_LIBRARIES}
  ${GNURADIO_FILTER_LIBRARIES}
  ${GNURADIO_DIGITAL_LIBRARIES}
  ${GNURADIO_ANALOG_LIBRARIES}
  ${GNURADIO_AUDIO_LIBRARIES}
  ${GNURADIO_UHD_LIBRARIES}
  ${UHD_LIBRARIES}
  ${GNURADIO_BLOCKS_LIBRARIES}
  ${GNURADIO_OSMOSDR_LIBRARIES}
  ${LIBOP25_REPEATER_LIBRARIES}
  gnuradio-op25_repeater
  )

if(NOT Gnuradio_VERSION VERSION_LESS "3.8")
  target_link_libraries(radio_scout_status
    gnuradio::gnuradio-analog
    gnuradio::gnuradio-blocks
    gnuradio::gnuradio-digital
    gnuradio::gnuradio-filter
    gnuradio::gnuradio-pmt
    )
endif()

install(TARGETS radio_scout_status
  LIBRARY DESTINATION ${CMAKE_INSTALL_PREFIX}/lib/trunk-recorder)
