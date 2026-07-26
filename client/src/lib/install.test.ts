import { describe, expect, it, vi } from 'vitest'

import {
  beIosSafari,
  beStandalone,
  completeInstall,
  offerInstall,
} from '@/test/install'
import { fakeStorage, hostileStorage } from '@/test/storage'

import { createInstall } from './install'

describe('install', () => {
  it('offers the browser prompt once the browser volunteers one', async () => {
    const install = createInstall()
    expect(install.offer).toBe('none')

    const event = offerInstall()

    expect(install.offer).toBe('prompt')
    expect(await install.install()).toBe('accepted')
    expect(event.prompted).toBe(1)
  })

  // The platform that most needs installing is the one with no install dialog:
  // iOS reaches Add-to-Home-Screen only through the Share sheet, so all we can
  // do is show the steps.
  it('offers the manual steps on iOS Safari, which fires no such event', () => {
    beIosSafari()

    expect(createInstall().offer).toBe('manual')
  })

  it('offers nothing once installed', () => {
    beIosSafari({ installed: true })

    expect(createInstall().offer).toBe('none')
  })

  it('offers nothing when already running standalone', () => {
    beStandalone()
    const install = createInstall()

    // The browser may still volunteer a prompt (a tab open beside the
    // installed app); running installed is the answer that wins.
    offerInstall()

    expect(install.offer).toBe('none')
  })

  // The browser decides *when* a page qualifies to be installed, which is
  // rarely the moment it loaded — so the banner has to be told, not asked once.
  it('tells a subscriber whenever the offer changes', () => {
    const install = createInstall({ storage: fakeStorage() })
    const changed = vi.fn()
    const unsubscribe = install.subscribe(changed)

    offerInstall()
    expect(changed).toHaveBeenCalledTimes(1)

    install.dismiss()
    expect(changed).toHaveBeenCalledTimes(2)

    unsubscribe()
    offerInstall()
    expect(changed).toHaveBeenCalledTimes(2)
  })

  // The tab that asked is still open and still showing the banner; the display
  // mode it reads won't change, because *this* window is still a browser tab.
  it('stops offering the moment the install completes', () => {
    const install = createInstall({ storage: fakeStorage() })
    const changed = vi.fn()
    install.subscribe(changed)
    offerInstall()

    completeInstall()

    expect(install.offer).toBe('none')
    expect(changed).toHaveBeenCalledTimes(2)
  })

  // A `BeforeInstallPromptEvent` is single-use: the browser will not let the
  // same one open a second dialog, so the banner must not offer one.
  it('takes a no from the browser dialog as a no, and asks no more', async () => {
    const storage = fakeStorage()
    const install = createInstall({ storage })
    offerInstall('dismissed')

    expect(await install.install()).toBe('dismissed')
    expect(install.offer).toBe('none')

    const reloaded = createInstall({ storage })
    offerInstall()
    expect(reloaded.offer).toBe('none')
  })

  it('reports having nothing to show when asked to prompt without one', async () => {
    const install = createInstall({ storage: fakeStorage() })

    expect(await install.install()).toBe('dismissed')
  })

  describe('dismissal', () => {
    it('is remembered, so the banner asks once and not again', () => {
      const storage = fakeStorage()
      const install = createInstall({ storage })
      offerInstall()

      install.dismiss()

      expect(install.offer).toBe('none')
      // A fresh page load — the browser will volunteer the prompt again, and
      // the answer must still be no.
      const reloaded = createInstall({ storage })
      offerInstall()
      expect(reloaded.offer).toBe('none')
    })

    // Private mode throws on both read and write. A listener who can't be
    // remembered still gets a working scanner — and a banner they can close
    // for this visit.
    it('survives a browser that refuses to remember it', () => {
      const install = createInstall({ storage: hostileStorage })
      offerInstall()

      install.dismiss()

      expect(install.offer).toBe('none')
    })
  })
})
