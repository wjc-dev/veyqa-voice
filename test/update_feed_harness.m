#import "../native_settings/VeyqaUpdates.m"

@interface FeedTestGate : NSObject <VeyqaUpdateGate>
@property BOOL ready;
@end
@implementation FeedTestGate
- (BOOL)updateMayCheck { return self.ready; }
- (BOOL)updatePrepareToInstall { return self.ready; }
- (void)updateDidCancel {}
- (void)updateWillRelaunch {}
@end

@interface FeedTestUpdater : NSObject
@property BOOL sessionInProgress;
@property BOOL automaticallyChecksForUpdates;
@property(copy) NSDictionary *httpHeaders;
@property NSInteger manualChecks;
@property NSInteger backgroundChecks;
@property NSInteger informationChecks;
@end
@implementation FeedTestUpdater
- (void)checkForUpdates { self.manualChecks++; }
- (void)checkForUpdatesInBackground { self.backgroundChecks++; }
- (void)checkForUpdateInformation { self.informationChecks++; }
@end

@interface FeedTestBundle : NSObject
@property(copy) NSString *feed;
@end
@implementation FeedTestBundle
- (id)objectForInfoDictionaryKey:(NSString *)key {
    return [key isEqual:@"SUFeedURL"] ? self.feed : @"test";
}
@end

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

int VeyqaRunFeedTests(const char *statePath) {
    @autoreleasepool {
        NSError *timeout = [NSError errorWithDomain:NSURLErrorDomain code:NSURLErrorTimedOut userInfo:nil];
        NSError *wrapped = [NSError errorWithDomain:SUSparkleErrorDomain code:SUAppcastError
            userInfo:@{NSUnderlyingErrorKey: timeout}];
        CHECK(VeyqaIsFeedConnectionError(wrapped));
        for (NSNumber *code in @[@(NSURLErrorCancelled), @(NSURLErrorSecureConnectionFailed),
                                 @(NSURLErrorServerCertificateUntrusted), @(NSURLErrorNotConnectedToInternet)]) {
            CHECK(!VeyqaIsFeedConnectionError([NSError errorWithDomain:NSURLErrorDomain code:code.integerValue userInfo:nil]));
        }
        for (NSNumber *code in @[@(SUSignatureError), @(SUValidationError), @(SUInstallationError)]) {
            CHECK(!VeyqaIsFeedConnectionError([NSError errorWithDomain:SUSparkleErrorDomain code:code.integerValue
                userInfo:@{NSUnderlyingErrorKey: timeout}]));
        }
        VeyqaUpdateController *controller = [VeyqaUpdateController new];
        controller.stateURL = [NSURL fileURLWithPath:[NSString stringWithUTF8String:statePath]];
        FeedTestBundle *bundle = [FeedTestBundle new];
        bundle.feed = VeyqaPrimaryFeed;
        FeedTestGate *gate = [FeedTestGate new];
        FeedTestUpdater *updater = [FeedTestUpdater new];
        controller.host = (NSBundle *)bundle;
        controller.gate = gate;
        controller.updater = (SPUUpdater *)updater;
        CHECK([controller retryFeedAfterError:wrapped]);
        [controller resumeIfReady];
        CHECK(updater.manualChecks == 0 && controller.retryFeedPending);
        gate.ready = YES;
        updater.sessionInProgress = YES;
        [controller resumeIfReady];
        CHECK(updater.manualChecks == 0 && controller.retryFeedPending);
        updater.sessionInProgress = NO;
        [controller resumeIfReady];
        CHECK(updater.manualChecks == 1 && controller.usingFallback && !controller.retryFeedPending);
        CHECK([[controller feedURLStringForUpdater:(SPUUpdater *)updater] isEqual:VeyqaFallbackFeed]);
        CHECK(![controller retryFeedAfterError:wrapped]);
        [controller updater:(SPUUpdater *)updater didFinishLoadingAppcast:(SUAppcast *)[NSObject new]];
        CHECK(updater.httpHeaders == nil);
        controller.usingFallback = NO;
        CHECK(![controller retryFeedAfterError:wrapped]); // No retry once the feed loaded.
        controller.feedLoaded = NO;
        bundle.feed = @"https://example.test/feed.xml";
        CHECK(![controller retryFeedAfterError:wrapped]);
        bundle.feed = VeyqaPrimaryFeed;
        controller.retryCheck = SPUUpdateCheckUpdatesInBackground;
        CHECK([controller retryFeedAfterError:wrapped]);
        [controller resumeIfReady];
        CHECK(updater.backgroundChecks == 1 && updater.manualChecks == 1);
        controller.usingFallback = NO;
        controller.retryCheck = SPUUpdateCheckUpdateInformation;
        CHECK([controller retryFeedAfterError:wrapped]);
        [controller resumeIfReady];
        CHECK(updater.informationChecks == 1);
        return 0;
    }
}
