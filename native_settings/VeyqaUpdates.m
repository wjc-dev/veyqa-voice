// Sparkle owns downloads, signature verification, installation, and restart.
// This adapter only coordinates those operations with Veyqa's live session.
#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
#import "VeyqaUpdates.h"

@interface VeyqaUpdateController ()
@property(nonatomic, strong) SPUUpdater *updater;
@property(nonatomic, strong) id<VeyqaUpdateGate> gate;
@property(nonatomic, strong) NSURL *stateURL;
@property(nonatomic, strong) NSBundle *host;
@property(nonatomic, copy) NSString *status;
@property(nonatomic, copy) NSDictionary *lastState;
@property(nonatomic, copy) void (^pendingInstall)(void);
@property(nonatomic) BOOL pendingCheck;
@property(nonatomic) BOOL started;
@property(nonatomic) BOOL relaunching;
@end

@implementation VeyqaUpdateController
- (instancetype)initWithBundlePath:(NSString *)bundlePath dataPath:(NSString *)dataPath gate:(id<VeyqaUpdateGate>)gate {
    if ((self = [super init])) {
        _host = [NSBundle bundleWithPath:bundlePath];
        if (!_host) return nil;
        _gate = gate;
        _stateURL = [[NSURL fileURLWithPath:dataPath isDirectory:YES] URLByAppendingPathComponent:@"updates.json"];
        if ([[_host objectForInfoDictionaryKey:@"VeyqaDistribution"] isEqual:@"private-trial"]) {
            _status = @"私测版本由分发者提供更新";
            return self;
        }
        SPUStandardUserDriver *driver = [[SPUStandardUserDriver alloc] initWithHostBundle:_host delegate:nil];
        _updater = [[SPUUpdater alloc] initWithHostBundle:_host applicationBundle:_host userDriver:driver delegate:self];
        _status = @"";
    }
    return self;
}

- (void)publish:(NSString *)status {
    self.status = status;
    NSDictionary *state = @{@"status": status, @"available": @(self.started),
        @"automatic": @(self.updater.automaticallyChecksForUpdates),
        @"checking": @(self.updater.sessionInProgress),
        @"version": [self.host objectForInfoDictionaryKey:@"CFBundleShortVersionString"] ?: @"",
        @"build": [self.host objectForInfoDictionaryKey:@"CFBundleVersion"] ?: @""};
    if ([state isEqual:self.lastState]) return;
    NSData *data = [NSJSONSerialization dataWithJSONObject:state options:0 error:nil];
    if ([data writeToURL:self.stateURL options:NSDataWritingAtomic error:nil]) {
        [[NSFileManager defaultManager] setAttributes:@{NSFilePosixPermissions: @0600} ofItemAtPath:self.stateURL.path error:nil];
        self.lastState = state;
    }
}

- (void)start {
    if ([[self.host objectForInfoDictionaryKey:@"VeyqaDistribution"] isEqual:@"private-trial"]) {
        [self publish:@"私测版本由分发者提供更新，不连接公开更新源"];
        return;
    }
    NSError *error = nil;
    self.started = [self.updater startUpdater:&error];
    [self publish:self.started ? @"自动检查新版，安装前由你确认" : @"暂时无法检查更新，请从官网下载新版"];
    if (error) NSLog(@"Veyqa updater start failed: %@", error);
}

- (void)checkForUpdates {
    if (!self.started) return;
    if (![self.gate updateMayCheck]) {
        self.pendingCheck = YES;
        [self publish:@"请先完成听写或保存设置，随后检查更新"];
        return;
    }
    self.pendingCheck = NO;
    [self publish:@"正在检查更新…"];
    [self.updater checkForUpdates];
}

- (void)setAutomaticChecks:(BOOL)enabled {
    if (!self.updater) return;
    self.updater.automaticallyChecksForUpdates = enabled;
    [self publish:enabled ? @"已开启每日检查更新" : @"已关闭自动检查，可手动检查更新"];
}

- (void)resumeIfReady {
    if (self.pendingInstall && [self.gate updatePrepareToInstall]) {
        void (^install)(void) = self.pendingInstall;
        self.pendingInstall = nil;
        [self publish:@"正在安装，即将重新打开…"];
        install();
    } else if (self.pendingCheck && [self.gate updateMayCheck]) {
        [self checkForUpdates];
    }
}

- (BOOL)updater:(SPUUpdater *)updater mayPerformUpdateCheck:(SPUUpdateCheck)check error:(NSError **)error {
    if ([self.gate updateMayCheck]) return YES;
    if (error) *error = [NSError errorWithDomain:@"com.wjcdev.veyqa.updates" code:1
        userInfo:@{NSLocalizedDescriptionKey: @"请先完成听写或保存设置，再检查更新。"}];
    return NO;
}

- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item {
    [self publish:[NSString stringWithFormat:@"发现新版 %@", item.displayVersionString]];
}

- (void)updaterDidNotFindUpdate:(SPUUpdater *)updater error:(NSError *)error {
    [self publish:@"当前已是最新版本"];
}

- (void)updater:(SPUUpdater *)updater willDownloadUpdate:(SUAppcastItem *)item withRequest:(NSMutableURLRequest *)request {
    [self publish:@"正在下载新版…"];
}

- (BOOL)updater:(SPUUpdater *)updater shouldPostponeRelaunchForUpdate:(SUAppcastItem *)item untilInvokingBlock:(void (^)(void))install {
    if ([self.gate updatePrepareToInstall]) return NO;
    self.pendingInstall = install;
    [self publish:@"新版已就绪：完成听写并保存设置后，自动继续安装"];
    return YES;
}

- (void)updaterWillRelaunchApplication:(SPUUpdater *)updater {
    self.relaunching = YES;
    [self.gate updateWillRelaunch];
}

- (void)updater:(SPUUpdater *)updater didAbortWithError:(NSError *)error {
    self.relaunching = NO;
    self.pendingInstall = nil;
    [self.gate updateDidCancel];
    if (error.code != SUNoUpdateError) {
        [self publish:@"更新未完成，当前版本可继续使用；请重试或从官网下载"];
        NSLog(@"Veyqa update aborted: %@", error);
    }
}

- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    if (!self.pendingInstall && !self.relaunching) [self.gate updateDidCancel];
    [self publish:self.status];
}
@end
