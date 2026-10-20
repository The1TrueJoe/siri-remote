// Read-only: GET the feature report (ID 0xFF) of each of the Siri Remote's
// virtual HID devices. Apple's driver translates each into a "keyhole" GET to
// the remote and logs the hidden report ID it used, which maps virtual device
// -> keyhole report. Nothing is written to the remote.
import Foundation
import IOKit.hid

setvbuf(stdout, nil, _IOLBF, 0)
let mgr = IOHIDManagerCreate(kCFAllocatorDefault, IOOptionBits(kIOHIDOptionsTypeNone))
IOHIDManagerSetDeviceMatching(mgr, [kIOHIDVendorIDKey: 76, kIOHIDProductIDKey: 614] as CFDictionary)
let rc = IOHIDManagerOpen(mgr, IOOptionBits(kIOHIDOptionsTypeNone))
print(String(format: "open -> 0x%08x", UInt32(bitPattern: rc)))
guard let devs = IOHIDManagerCopyDevices(mgr) as? Set<IOHIDDevice> else { exit(1) }

func prop(_ d: IOHIDDevice, _ k: String) -> Int {
    (IOHIDDeviceGetProperty(d, k as CFString) as? NSNumber)?.intValue ?? -1
}

for d in devs.sorted(by: { prop($0, kIOHIDPrimaryUsagePageKey) * 65536 + prop($0, kIOHIDPrimaryUsageKey)
                         < prop($1, kIOHIDPrimaryUsagePageKey) * 65536 + prop($1, kIOHIDPrimaryUsageKey) }) {
    let page = prop(d, kIOHIDPrimaryUsagePageKey), usage = prop(d, kIOHIDPrimaryUsageKey)
    var len = CFIndex(512)
    let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: 512)
    buf.initialize(repeating: 0, count: 512)
    let t0 = Date()
    let r = IOHIDDeviceGetReport(d, kIOHIDReportTypeFeature, 0xFF, buf, &len)
    let ms = Int(Date().timeIntervalSince(t0) * 1000)
    let hex = (0..<min(len, 48)).map { String(format: "%02x", buf[$0]) }.joined(separator: " ")
    print(String(format: "page 0x%04x usage 0x%04x  GET feature 0xFF -> 0x%08x  len %d  (%d ms)",
                 page, usage, UInt32(bitPattern: r), len, ms))
    if r == 0 { print("    \(hex)") }
    Thread.sleep(forTimeInterval: 0.3)
}
