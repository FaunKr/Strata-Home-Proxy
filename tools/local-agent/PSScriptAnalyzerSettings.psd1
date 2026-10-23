# PSScriptAnalyzer settings for the PowerShell runner (scripts/check.sh): the
# default rules, plus a check that it runs on Windows PowerShell 5.1, which
# ships with Windows, although CI only has PowerShell 7.
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
