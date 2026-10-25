import Foundation

/// Standalone executable checks so the repository's Swift project does not need
/// a separate XCTest target just to exercise the pure URL and response parsing.
@main
enum ArtistSignatureRepositoryTests {
    static func main() throws {
        try testBlankOverrideBuildsCommonsSearchAt1600Pixels()
        try testSearchTermEncodesReservedCharacters()
        try testParseSelectsMatchingSvgThumbnail()
        try testParseRejectsUnrelatedOrMalformedResults()
        try testMissingThumbnailIsNotFound()
        print("artist signature repository tests passed")
    }

    private static func testBlankOverrideBuildsCommonsSearchAt1600Pixels() throws {
        let url = ArtistSignatureRepository.requestURL(artistName: "Björk", endpointOverride: "")
        try expect(url?.host == "commons.wikimedia.org", "blank override must use Commons")
        let components = url.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false) }
        try expect(components?.queryItems?.first(where: { $0.name == "iiurlwidth" })?.value == "1600", "Commons request must request 1600px thumbnails")
        try expect(components?.queryItems?.first(where: { $0.name == "gsrsearch" })?.value == "Björk signature OR autograph", "search term must be retained")
    }

    private static func testSearchTermEncodesReservedCharacters() throws {
        let url = ArtistSignatureRepository.requestURL(artistName: "A+B & C=2", endpointOverride: "")
        let encodedQuery = url.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false)?.percentEncodedQuery } ?? ""
        try expect(encodedQuery.contains("A%2BB%20%26%20C%3D2%20signature%20OR%20autograph"), "+, & and = in artist names must be percent-encoded")
    }

    private static func testParseSelectsMatchingSvgThumbnail() throws {
        let response = #"{"query":{"pages":{"1":{"title":"File:Björk_signature.svg","imageinfo":[{"url":"https://upload.wikimedia.org/original.svg","thumburl":"https://upload.wikimedia.org/thumb.png","mime":"image/svg+xml"}]},"2":{"title":"File:Björk_photo.jpg","imageinfo":[{"url":"https://upload.wikimedia.org/photo.jpg","mime":"image/jpeg"}]}}}}"#
        guard case .found(let result) = ArtistSignatureRepository.parseResponse(Data(response.utf8), requestedArtist: "Björk") else {
            throw CheckFailure.failed("parser must find the matching SVG thumbnail")
        }
        try expect(result.artistName == "Björk", "result must retain the requested artist")
        try expect(result.imageURL.absoluteString == "https://upload.wikimedia.org/thumb.png", "parser must select the thumbnail URL")
        try expect(result.isSVG, "SVG signature format must be identified")
    }

    private static func testParseRejectsUnrelatedOrMalformedResults() throws {
        let response = #"{"query":{"pages":{"1":{"title":"File:Other Artist signature.svg","imageinfo":[{"url":"https://upload.wikimedia.org/signature.svg","mime":"image/svg+xml"}]}}}}"#
        try expect(ArtistSignatureRepository.parseResponse(Data(response.utf8), requestedArtist: "Björk") == .notFound, "unrelated artists must not be selected")
        try expect(ArtistSignatureRepository.parseResponse(Data("{}".utf8), requestedArtist: "Björk") == .failed, "malformed responses must be errors")
    }

    private static func testMissingThumbnailIsNotFound() throws {
        let response = #"{"query":{"pages":{"1":{"title":"File:Björk_signature.svg","imageinfo":[{"url":"https://upload.wikimedia.org/original.svg","mime":"image/svg+xml"}]}}}}"#
        try expect(ArtistSignatureRepository.parseResponse(Data(response.utf8), requestedArtist: "Björk") == .notFound, "raw SVGs without a bounded thumbnail must be skipped")
    }

    private static func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
        guard condition() else { throw CheckFailure.failed(message) }
    }
}

private enum CheckFailure: Error {
    case failed(String)
}
