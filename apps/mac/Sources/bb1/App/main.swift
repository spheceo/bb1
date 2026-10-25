import AppKit
import BB1Core

// bb1                     menu bar app; mode (host/client) comes from settings, asked on first run
// bb1 host  [options]     host mode (--headless: no UI, for testing)
// bb1 client [options]    client mode (--headless: decode and print stats, for testing)

let usage = """
usage: bb1                    run the app (mode from settings)
       bb1 host [options]     run as host   (bb1 host --help)
       bb1 client [options]   run as client (bb1 client --help)
"""

func runApp(mode: AppMode?, host: HostOptions, client: ClientOptions) -> Never {
    let app = NSApplication.shared
    let delegate = AppDelegate(forcedMode: mode, hostOptions: host, clientOptions: client)
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    app.run()
    exit(0)
}

let args = CommandLine.arguments
switch args.count > 1 ? args[1] : "" {
case "host":
    let options = HostOptions.parse(["bb1 host"] + args.dropFirst(2))
    if options.headless {
        Log.setup("host")
        let host = HostController(options: options)
        print("bb1 host — pairing key: \(host.key.display)")
        host.start()
        dispatchMain()
    }
    runApp(mode: .host, host: options, client: ClientOptions())

case "client":
    let options = ClientOptions.parse(["bb1 client"] + args.dropFirst(2))
    if options.headless {
        Log.setup("client")
        setvbuf(stdout, nil, _IOLBF, 0)
        guard let target = options.target() else {
            print("--headless needs --host and --key\n\(ClientOptions.usage)")
            exit(2)
        }
        let headless = HeadlessClient(target: target, seconds: options.seconds)
        headless.run()
        dispatchMain()
    }
    runApp(mode: .client, host: HostOptions(), client: options)

case "-h", "--help", "help":
    print(usage)
    exit(0)

default:
    // Launched from Finder/launchd (macOS may add -psn_… arguments).
    runApp(mode: nil, host: HostOptions(), client: ClientOptions())
}
