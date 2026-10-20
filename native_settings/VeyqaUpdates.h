#import <Foundation/Foundation.h>
#import <Sparkle/Sparkle.h>

@protocol VeyqaUpdateGate <NSObject>
- (BOOL)updateMayCheck;
- (BOOL)updatePrepareToInstall;
- (void)updateDidCancel;
- (void)updateWillRelaunch;
@end

@interface VeyqaUpdateController : NSObject <SPUUpdaterDelegate>
- (instancetype)initWithBundlePath:(NSString *)bundlePath dataPath:(NSString *)dataPath gate:(id<VeyqaUpdateGate>)gate;
- (void)start;
- (void)checkForUpdates;
- (void)setAutomaticChecks:(BOOL)enabled;
- (void)resumeIfReady;
@end
