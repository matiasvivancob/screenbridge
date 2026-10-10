// Menu-bar app: no Dock icon, one status item, all work done by Agent.

import AppKit
import ServiceManagement

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    let agent = Agent()
    var status = "Waiting for a PC"

    func applicationDidFinishLaunching(_ note: Notification) {
        setIcon(active: false)
        let menu = NSMenu()
        menu.delegate = self
        item.menu = menu
        agent.onStatus = { [weak self] text, active in
            self?.status = text
            self?.setIcon(active: active)
        }
        agent.approve = { [weak self] name, done in self?.askToAllow(name, done) }
        // Ask for Screen Recording up front so the first Extend from the PC just works.
        if !CGPreflightScreenCaptureAccess() { CGRequestScreenCaptureAccess() }
        agent.start()
    }

    func setIcon(active: Bool) {
        let img = NSImage(systemSymbolName: active ? "display.2" : "rectangle.on.rectangle", accessibilityDescription: "ScreenBridge")
        img?.isTemplate = true
        item.button?.image = img
    }

    func askToAllow(_ name: String, _ done: @escaping (Bool) -> Void) {
        NSApp.activate(ignoringOtherApps: true)
        let a = NSAlert()
        a.messageText = "Allow “\(name)” to use this Mac's displays?"
        a.informativeText = "\(name) will create extra displays on this Mac and receive everything shown on them. Only allow computers you trust. You can remove it later from the ScreenBridge menu."
        a.addButton(withTitle: "Allow")
        a.addButton(withTitle: "Don't Allow")
        done(a.runModal() == .alertFirstButtonReturn)
    }

    // Rebuilt every time the menu opens, so it always shows current state.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let head = NSMenuItem(title: status, action: nil, keyEquivalent: "")
        head.isEnabled = false
        menu.addItem(head)
        if status.hasPrefix("Extending") {
            menu.addItem(withTitle: "Stop Extending", action: #selector(stopSession), keyEquivalent: "").target = self
        }
        if !CGPreflightScreenCaptureAccess() {
            menu.addItem(withTitle: "Grant Screen Recording Permission…", action: #selector(openPrivacy), keyEquivalent: "").target = self
        }
        menu.addItem(.separator())

        let pcs = NSMenuItem(title: "Allowed PCs", action: nil, keyEquivalent: "")
        let sub = NSMenu()
        let allowed = agent.allowed
        if allowed.isEmpty {
            let none = NSMenuItem(title: "None yet: a PC asks the first time it connects", action: nil, keyEquivalent: "")
            none.isEnabled = false
            sub.addItem(none)
        }
        for (id, name) in allowed.sorted(by: { $0.value < $1.value }) {
            let i = NSMenuItem(title: "Remove “\(name)”", action: #selector(revoke(_:)), keyEquivalent: "")
            i.representedObject = id
            i.target = self
            sub.addItem(i)
        }
        pcs.submenu = sub
        menu.addItem(pcs)

        let login = NSMenuItem(title: "Open at Login", action: #selector(toggleLogin), keyEquivalent: "")
        login.target = self
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        menu.addItem(login)
        menu.addItem(.separator())
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev"
        let about = NSMenuItem(title: "ScreenBridge \(version) · \(agent.name)", action: nil, keyEquivalent: "")
        about.isEnabled = false
        menu.addItem(about)
        menu.addItem(withTitle: "Quit ScreenBridge", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    }

    @objc func stopSession() { agent.stop() }

    @objc func revoke(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        agent.allowed.removeValue(forKey: id)
    }

    @objc func toggleLogin() {
        do {
            if SMAppService.mainApp.status == .enabled { try SMAppService.mainApp.unregister() } else { try SMAppService.mainApp.register() }
        } catch {
            let a = NSAlert(error: error)
            a.informativeText = "Move ScreenBridge to the Applications folder and try again."
            a.runModal()
        }
    }

    @objc func openPrivacy() {
        CGRequestScreenCaptureAccess()
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")!)
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
