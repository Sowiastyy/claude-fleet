use framework "AppKit"
use scripting additions
on run argv
	set out to item 1 of argv
	set ca to current application
	set pb to ca's NSPasteboard's generalPasteboard()
	set opts to ca's NSDictionary's dictionaryWithObject:true forKey:(ca's NSPasteboardURLReadingFileURLsOnlyKey)
	set urls to pb's readObjectsForClasses:{ca's NSURL} options:opts
	if urls is not missing value and (urls's |count|()) > 0 then
		return ((urls's valueForKey:"path")'s componentsJoinedByString:linefeed) as text
	end if
	set png to pb's dataForType:(ca's NSPasteboardTypePNG)
	if png is missing value then
		set tiff to pb's dataForType:(ca's NSPasteboardTypeTIFF)
		if tiff is missing value then return ""
		set rep to ca's NSBitmapImageRep's imageRepWithData:tiff
		set png to rep's representationUsingType:(ca's NSBitmapImageFileTypePNG) |properties|:(ca's NSDictionary's dictionary())
	end if
	png's writeToFile:out atomically:true
	return out
end run
