# PSScriptAnalyzer settings for the PowerShell runner (scripts/check.sh): the
# default rules, plus a check that its syntax and commands exist in Windows
# PowerShell 5.1, which ships with Windows, although CI only has PowerShell 7.
# .NET calls are not checked: PSUseCompatibleTypes flagged no call missing
# from .NET Framework in a test.
@{
    # Rules for cmdlets in modules, not for a script's own helper functions.
    ExcludeRules = @(
        'PSUseShouldProcessForStateChangingFunctions',
        'PSUseSingularNouns',
        'PSAvoidUsingPositionalParameters'
    )
    Rules        = @{
        PSUseCompatibleSyntax   = @{
            Enable         = $true
            TargetVersions = @('5.1')
        }
        PSUseCompatibleCommands = @{
            Enable         = $true
            TargetProfiles = @('win-48_x64_10.0.17763.0_5.1.17763.316_x64_4.0.30319.42000_framework')
        }
    }
}
