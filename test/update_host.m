// Isolated integration host: no microphone, TCC, credentials, or login service.
#import <AppKit/AppKit.h>
#import "../native_settings/VeyqaUpdates.h"

@interface UpdateTestHost : NSObject <NSApplicationDelegate, VeyqaUpdateGate>
@property(nonatomic, strong) VeyqaUpdateController *updates;
@property(nonatomic, strong) NSWindow *window;
@property(nonatomic, strong) NSButton *busy;
@end

@implementation UpdateTestHost
- (void)applicationDidFinishLaunching:(NSNotification *)note {
    NSString *dataPath = [[[NSBundle mainBundle] bundlePath] stringByDeletingLastPathComponent];
    NSString *version = [[NSBundle mainBundle] objectForInfoDictionaryKey:@"CFBundleVersion"];
    [version writeToFile:[dataPath stringByAppendingPathComponent:@"launched-build.txt"] atomically:YES encoding:NSUTF8StringEncoding error:nil];
    self.updates = [[VeyqaUpdateController alloc] initWithBundlePath:[NSBundle mainBundle].bundlePath dataPath:dataPath gate:self];
    [self.updates start];
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 440, 180)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
    self.window.title = [NSString stringWithFormat:@"Veyqa 更新测试 · 构建 %@", version];
    NSTextField *label = [NSTextField labelWithString:@"隔离测试：不读取 Key，不申请麦克风权限"];
    label.frame = NSMakeRect(24, 125, 400, 24);
    [self.window.contentView addSubview:label];
    self.busy = [NSButton checkboxWithTitle:@"模拟正在听写" target:nil action:nil];
    self.busy.frame = NSMakeRect(24, 80, 300, 28);
    [self.window.contentView addSubview:self.busy];
    NSButton *check = [NSButton buttonWithTitle:@"检查更新" target:self action:@selector(check:)];
    check.frame = NSMakeRect(24, 30, 140, 34);
    [self.window.contentView addSubview:check];
    [NSTimer scheduledTimerWithTimeInterval:0.5 repeats:YES block:^(NSTimer *timer) { [self.updates resumeIfReady]; }];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
}
- (void)check:(id)sender { [self.updates checkForUpdates]; }
- (BOOL)updateMayCheck { return self.busy.state != NSControlStateValueOn; }
- (BOOL)updatePrepareToInstall { return self.busy.state != NSControlStateValueOn; }
- (void)updateDidCancel {}
- (void)updateWillRelaunch {}
- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)sender { return YES; }
@end

int main(void) {
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        UpdateTestHost *delegate = [UpdateTestHost new];
        app.delegate = delegate;
        [app run];
    }
    return 0;
}
