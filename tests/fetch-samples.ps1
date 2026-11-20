# Optional online setup for the real-document regression suite. The converter runs offline.
$ErrorActionPreference = 'Stop'
$destination = Join-Path $PSScriptRoot '..\target\corpus'
New-Item -ItemType Directory -Force $destination | Out-Null
$base = 'https://raw.githubusercontent.com/apache/tika/2487f2b125d3e908808a1f450ed8e83e5869fc87/tika-parsers/tika-parsers-standard/tika-parsers-standard-modules'
$groups = @{
    microsoft = @('testEXCEL.xls','testEXCEL.xlsx','testPPT.ppt','testPPT.pptx','testWORD.doc','testWORD.docx','testRTF.rtf')
    miscoffice = @('testEPUB.epub','testHWP_5.0.hwp','testOpenOffice2.odt','versions/LibreOfficeCalc_ods_1.3.ods','versions/LibreOfficeImpress_odp_1.3.odp')
    pdf = @('testPDF.pdf')
}
foreach ($group in $groups.Keys) {
    foreach ($file in $groups[$group]) {
        $url = "$base/tika-parser-$group-module/src/test/resources/test-documents/$file"
        Invoke-WebRequest $url -OutFile (Join-Path $destination (Split-Path $file -Leaf))
    }
}
Invoke-WebRequest 'https://raw.githubusercontent.com/airmang/python-hwpx/679225782cc07c2fe0c265bc1208fa679a266c8e/examples/FormattingShowcase.hwpx' -OutFile (Join-Path $destination 'FormattingShowcase.hwpx')
